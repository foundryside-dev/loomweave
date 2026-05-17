"""JSONL events sidecar recorder for the standalone composer MCP server.

Every dispatch through ``_dispatch_tool`` records one
:class:`ComposerToolInvocation` line to
``{scratch_dir}/{session_id}.events.jsonl``. The sidecar is append-only;
each line is one canonical-JSON record.

Pre-resolution buffering
------------------------
Standalone-MCP sessions resolve their ``session_id`` lazily — the closure-
captured ``session_id_ref`` in ``server.create_server`` is ``None`` until
the first ``new_session`` (or ``load_session``) call returns success.
The recorder takes a callable that reads ``session_id_ref`` on every
``record()`` and buffers in memory until that callable returns a concrete
value. The server explicitly calls :meth:`resolve_session` after setting
``session_id_ref[0]`` so the buffer is drained to the correct sidecar
even if the LLM emits no further tool calls.

Locking discipline
------------------
The recorder uses its own per-session ``threading.Lock`` (NOT the
:class:`~elspeth.composer_mcp.session.SessionManager` lock) — those guard
the canonical session JSON write contract (version-check + atomic
replace), which the events sidecar does not need. Reusing them would
serialize every tool dispatch behind any in-progress whole-session
save. Cross-process safety is out of scope: the standalone MCP server
is single-tenant per scratch dir.

``record()`` writes are durable on return:
``open("a") → write → flush → os.fsync → close``. ``CompactingWriteAheadLog``
patterns are not used here — JSONL is the natural shape for this audit
log because each line is a complete, self-describing record.

Layer: L3 (application). Imports L0 (contracts.composer_audit) and stdlib only.
"""

from __future__ import annotations

import json
import os
import threading
from collections.abc import Callable
from pathlib import Path

from elspeth.contracts.composer_audit import ComposerToolInvocation, ComposerToolRecorder

__all__ = ["JsonlEventRecorder", "events_sidecar_path", "verify_events_sidecar_integrity"]


# Per-session lock registry. Mirrors the pattern in ``composer_mcp/session.py``
# but is a SEPARATE registry — these locks guard appends to the events sidecar,
# not the canonical session JSON file. Sharing the registry would serialize
# every tool dispatch behind any in-progress whole-session save.
_EVENT_LOCKS: dict[str, threading.Lock] = {}
_EVENT_LOCKS_REGISTRY_MUTEX = threading.Lock()


def _event_lock(session_id: str) -> threading.Lock:
    """Return the process-local mutex guarding one session's events sidecar."""
    if session_id in _EVENT_LOCKS:
        return _EVENT_LOCKS[session_id]
    with _EVENT_LOCKS_REGISTRY_MUTEX:
        if session_id not in _EVENT_LOCKS:
            _EVENT_LOCKS[session_id] = threading.Lock()
        return _EVENT_LOCKS[session_id]


def events_sidecar_path(scratch_dir: Path, session_id: str) -> Path:
    """Return the canonical sidecar path for a session_id.

    Mirrors ``SessionManager._session_path`` shape — same scratch_dir,
    sibling file name with ``.events.jsonl`` suffix. ``SessionManager.delete``
    unlinks the sidecar via this helper to keep cleanup paths in lockstep.
    """
    return scratch_dir / f"{session_id}.events.jsonl"


class JsonlEventRecorder(ComposerToolRecorder):
    """Append-only JSONL recorder for composer tool dispatches.

    Args:
        scratch_dir: The MCP scratch dir (same as ``SessionManager``).
        session_id_provider: Callable returning the current session_id
            (``None`` until ``new_session``/``load_session`` resolves it).
            The recorder reads this on every ``record()`` and on
            :meth:`resolve_session`.

    Threading: ``record()`` is safe to call from any thread; per-session
    locks serialize concurrent appends to the same sidecar. The buffer
    has its own lock to allow concurrent records-during-pre-resolution.
    """

    def __init__(
        self,
        scratch_dir: Path,
        session_id_provider: Callable[[], str | None],
    ) -> None:
        self._dir = scratch_dir
        self._provider = session_id_provider
        self._buffer: list[ComposerToolInvocation] = []
        self._buffer_lock = threading.Lock()
        # Tracks the session_id we have most recently flushed records to.
        # Once non-None, only changes if a load_session swaps to a different
        # session_id. delete_session keeps session_id_ref set until its
        # destructive success record is fsynced, then clears the active scope.
        self._flushed_session_id: str | None = None

    def record(self, invocation: ComposerToolInvocation) -> None:
        """Persist one invocation. Buffers if no session_id is resolved yet.

        **Pre-resolution data loss** (documented deliberately): records
        pushed before the first session_id resolution live in process
        memory. If the process exits before resolution (e.g. the very
        first ``new_session`` call raises and the MCP server dies), the
        buffered records are lost. This is acceptable for the standalone
        MCP — the records describe an attempt that itself failed before
        any session existed; there is no canonical session id to attach
        them to. The same dispatch's failure is observable from the LLM
        side (the ``CallToolResult`` carried ``isError=True``) and from
        upstream LiteLLM tracing. If durable pre-resolution audit becomes
        required, ``_pending_{pid}_{started_ns}.events.jsonl`` with an
        atomic rename on resolve is the canonical fix — flagged but not
        implemented.
        """
        sid = self._provider()
        if sid is None:
            with self._buffer_lock:
                self._buffer.append(invocation)
            return
        # Drain any pre-resolution buffer to THIS session's sidecar before
        # appending the new record. The buffer belongs to ``sid`` because
        # ``session_id_ref`` only flips from None to a concrete value once
        # — the buffer accumulated under that single provider transition.
        self._drain_buffer_to(sid)
        self._append([invocation], sid)

    def resolve_session(self, session_id: str) -> None:
        """Drain the pre-resolution buffer to ``session_id``'s sidecar.

        Called by the MCP server immediately after assigning
        ``session_id_ref[0]`` so the buffer flushes even if no further
        tool calls follow. Idempotent — repeated calls with the same
        ``session_id`` are no-ops once the buffer is drained.
        """
        self._drain_buffer_to(session_id)

    def _drain_buffer_to(self, session_id: str) -> None:
        # Hold ``_buffer_lock`` across the entire drain INCLUDING the
        # ``_append`` write — without this, a concurrent ``record()``
        # that arrives between the buffer-clear and the disk write can
        # acquire ``_event_lock`` first and append its record BEFORE
        # the drained batch lands, breaking the order invariant for
        # the audit log. ``_event_lock`` reentrance is not a concern
        # because they are different lock instances.
        with self._buffer_lock:
            if not self._buffer:
                self._flushed_session_id = session_id
                return
            to_flush = list(self._buffer)
            self._buffer.clear()
            self._append(to_flush, session_id)
            self._flushed_session_id = session_id

    def _append(self, invocations: list[ComposerToolInvocation], session_id: str) -> None:
        """Append one or more JSONL lines to the session's sidecar.

        fsync after each batch — audit primacy: the dispatch must not
        report success to the LLM without the audit record durable on
        disk. CLAUDE.md: "if it's not recorded, it didn't happen".
        """
        path = events_sidecar_path(self._dir, session_id)
        path.parent.mkdir(parents=True, exist_ok=True)
        # ``json.dumps`` with ``sort_keys=True`` produces a stable
        # serialization without depending on L1 ``canonical_json``. The
        # canonical-hash invariant lives on the ``arguments_hash`` /
        # ``result_hash`` fields inside the record, computed at the
        # dispatch site; the wrapping JSONL line is just a delivery
        # envelope and does not need RFC 8785 compliance.
        lines = [json.dumps(invocation.to_dict(), sort_keys=True, separators=(",", ":")) + "\n" for invocation in invocations]
        payload = "".join(lines)
        # Open / write / fsync / close pattern — one durability barrier
        # per batch. ``"a"`` mode positions the file pointer at EOF on
        # every open, so concurrent processes cannot interleave bytes
        # within a single ``write()`` of POSIX-atomic size; lines >
        # PIPE_BUF (4 KiB) are still safe under our process-local lock.
        # See module docstring on cross-process scope.
        with _event_lock(session_id), open(path, "a", encoding="utf-8") as f:
            f.write(payload)
            f.flush()
            os.fsync(f.fileno())


def verify_events_sidecar_integrity(path: Path) -> None:
    """Tier-1 read-back check for an events sidecar.

    **Semantics (deliberate, narrowed):** this verifier checks
    *byte-integrity* — for every line, ``arguments_hash`` and (when
    present) ``result_hash`` equal the SHA-256 of the stored
    ``arguments_canonical`` / ``result_canonical`` strings exactly as
    they appear on disk. It does NOT re-canonicalize: a tamper that
    rewrites both the canonical AND its hash to a non-RFC-8785 but
    consistent shape would pass this check.

    Why narrow: re-canonicalizing on read-back couples the verifier to
    every future change in :func:`~elspeth.core.canonical.canonical_json`
    (e.g., a domain-separation update would invalidate older sidecars).
    Audit primacy demands "the bytes I see hash to the digest I see" —
    that's preserved here. A separate check for canonical-form
    conformance can be layered on top by callers who want it; doing so
    inside the same verifier conflates two invariants.

    Mismatch raises ``ValueError`` with the offending line number.
    Silent coercion of the audit trail is evidence tampering.

    Operator/test usage:
    - ``mutate canonical without rehashing`` → crash on read-back (✓)
    - ``mutate hash without rewriting canonical`` → crash on read-back (✓)
    - ``mutate both consistently`` → not detected by this function (call
      site responsibility — combine with a canonical-form check if needed)
    """
    # hashlib is stdlib; no L1 import needed because the verifier is
    # checking "stored bytes hash to stored hash", not re-canonicalizing.
    import hashlib

    with path.open("r", encoding="utf-8") as f:
        for lineno, line in enumerate(f, start=1):
            if not line.strip():
                continue
            record = json.loads(line)
            args_canonical = record["arguments_canonical"]
            args_hash = record["arguments_hash"]
            recomputed = hashlib.sha256(args_canonical.encode("utf-8")).hexdigest()
            if recomputed != args_hash:
                raise ValueError(
                    f"Tier-1 audit anomaly at {path}:{lineno}: arguments_hash mismatch — "
                    f"recorded {args_hash!r}, recomputed {recomputed!r} from arguments_canonical."
                )
            # Direct access — ``ComposerToolInvocation.to_dict()`` always
            # emits both keys (with value None for ARG_ERROR/PLUGIN_CRASH
            # paths), so a missing key signals corruption (Tier 1) and
            # should crash with KeyError rather than be papered over.
            result_canonical = record["result_canonical"]
            result_hash = record["result_hash"]
            if (result_canonical is None) != (result_hash is None):
                raise ValueError(
                    f"Tier-1 audit anomaly at {path}:{lineno}: result digest pair incomplete — "
                    f"result_canonical and result_hash must both be null or both be present."
                )
            if result_canonical is not None and result_hash is not None:
                recomputed_r = hashlib.sha256(result_canonical.encode("utf-8")).hexdigest()
                if recomputed_r != result_hash:
                    raise ValueError(
                        f"Tier-1 audit anomaly at {path}:{lineno}: result_hash mismatch — "
                        f"recorded {result_hash!r}, recomputed {recomputed_r!r} from result_canonical."
                    )
