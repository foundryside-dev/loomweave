"""Composer tool-call audit primitives (L0).

Captures the per-tool-call decision trail produced when an LLM (or operator)
drives the elspeth-composer to build a pipeline graph. The completed pipeline
YAML is the artefact; this module's records are the *decision trail* — every
tool invocation that produced the artefact, including its arguments, result,
status, and version delta.

Two surfaces consume :class:`ComposerToolInvocation`:

1. The standalone composer MCP server appends one JSONL line per invocation
   to a per-session events sidecar.
2. The web composer service buffers invocations during the compose loop;
   the route handler persists each as a ``role=tool`` chat message with
   the audit sidecar carried inside the existing ``tool_calls`` JSON
   column under a ``_kind`` discriminator.

Layer: L0 (contracts). Imports nothing above. Canonical-JSON serialization
and SHA-256 hashing happen at L3 construction sites (recorders/dispatchers),
not from this module — that keeps the leaf clean.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from datetime import datetime
from enum import StrEnum
from typing import Any, Protocol


class ComposerToolStatus(StrEnum):
    """Outcome of a single composer tool dispatch.

    SUCCESS    — handler completed (the underlying tool may have returned
                 ``success=False`` semantically; that is still a successful
                 dispatch — the audit record carries the full result payload
                 so an auditor can read the semantic outcome).
    ARG_ERROR  — Tier-3 boundary failure. Either ``ToolArgumentError`` was
                 raised by a handler, or pre-dispatch validation rejected
                 the LLM-supplied arguments (JSON decode failure, non-dict
                 arguments, missing schema-required paths).
    PLUGIN_CRASH — any exception class other than ``ToolArgumentError``
                 escaped the handler. Per CLAUDE.md "Plugin Ownership"
                 this is a Tier-1/2 plugin bug; the audit record fixes
                 the time and arguments at which the bug fired.
    """

    SUCCESS = "success"
    ARG_ERROR = "arg_error"
    PLUGIN_CRASH = "plugin_crash"


@dataclass(frozen=True, slots=True)
class ComposerToolInvocation:
    """One composer tool dispatch as recorded for audit.

    Field semantics
    ---------------

    ``arguments_canonical`` / ``arguments_hash``
        RFC 8785 canonical JSON of the LLM-supplied arguments and its
        SHA-256 hex digest. ``arguments_hash == sha256(arguments_canonical)``
        is a Tier-1 invariant: a verifier reading this record back from
        durable storage MUST recompute the digest and crash on mismatch
        (silent coercion of the audit trail is evidence tampering).

    ``result_canonical`` / ``result_hash``
        Same pair for the dispatch result. ``None`` when the dispatch did
        not complete (``ARG_ERROR`` pre-dispatch sites, ``PLUGIN_CRASH``).

    ``status``
        See :class:`ComposerToolStatus`.

    ``error_class`` / ``error_message``
        Populated on ``ARG_ERROR`` and ``PLUGIN_CRASH``. ``error_message``
        is already-redacted at the dispatch boundary — for
        ``ToolArgumentError`` this is ``exc.args[0]``, which the structured
        constructor composes from the safe-by-design ``(argument, expected,
        actual_type)`` triple. For ``PLUGIN_CRASH`` callers MUST NOT pass
        ``str(exc)`` because plugin exception messages can carry secrets,
        DB URLs, or filesystem paths; pass only ``type(exc).__name__`` and
        a sanitized summary.

    ``version_before`` / ``version_after``
        :attr:`CompositionState.version` immediately before and after the
        dispatch. ``version_after is None`` on paths that did not complete
        (``ARG_ERROR`` pre-dispatch, ``PLUGIN_CRASH``). ``version_after ==
        version_before`` on cache hits and on dispatches that did not
        mutate state.

    ``cache_hit``
        ``True`` when the dispatch was served from the per-compose-call
        discovery cache without re-running the handler. Cache hits are
        recorded because the LLM made a *new* decision based on the
        cached payload; the original recording (when the cache was
        populated) belongs to a different decision.

    ``started_at`` / ``finished_at`` / ``latency_ms``
        UTC wall-clock window around the dispatch. ``latency_ms`` is
        derived from ``time.monotonic_ns`` at the dispatch site, not from
        the wall-clock pair — wall clocks can step backwards.

    ``actor``
        Stable string identifying who drove the dispatch.
        ``"composer-mcp:cli"`` or ``"composer-web:user-{user_id}"``.

    Immutability
    ------------
    Every field is a scalar, ``StrEnum``, ``datetime``, or ``str|None``;
    per the CLAUDE.md "Frozen Dataclass Immutability" → "Scalar-Only
    Fields Need No Guard" rule, ``frozen=True`` alone is sufficient.
    No ``__post_init__`` freeze guard is needed and none is defined —
    "Don't add guards that do nothing."
    """

    tool_call_id: str
    tool_name: str
    arguments_canonical: str
    arguments_hash: str
    result_canonical: str | None
    result_hash: str | None
    status: ComposerToolStatus
    error_class: str | None
    error_message: str | None
    version_before: int
    version_after: int | None
    started_at: datetime
    finished_at: datetime
    latency_ms: int
    actor: str
    cache_hit: bool = False

    def to_dict(self) -> dict[str, Any]:
        """JSON-friendly dict for sidecar serialization.

        ``started_at``/``finished_at`` are emitted as ISO-8601 strings so
        the dict is directly ``json.dumps``-able. ``status`` becomes its
        string value. The output shape is the canonical sidecar payload
        used by both standalone-MCP JSONL lines and web-composer
        ``role=tool`` chat-message ``tool_calls`` entries (under the
        ``_kind=audit`` discriminator).
        """
        raw = asdict(self)
        raw["status"] = self.status.value
        raw["started_at"] = self.started_at.isoformat()
        raw["finished_at"] = self.finished_at.isoformat()
        return raw


class ComposerToolRecorder(Protocol):
    """Append-only sink for :class:`ComposerToolInvocation` records.

    Implementations:

    - Standalone MCP: writes one JSONL line per invocation to a per-session
      events sidecar (``{scratch}/{session_id}.events.jsonl``). When the
      session_id is unresolved (the very first ``new_session`` call), the
      recorder buffers in memory and flushes on first resolution.
    - Web composer: in-memory buffer surfaced on
      :class:`ComposerResult` (and on partial-state errors) so the
      route handler can persist as ``role=tool`` chat messages inside
      the same DB transaction as the assistant message.

    Recorder calls happen synchronously from the dispatch site. Every
    code path through the dispatcher MUST call ``record(...)`` before
    returning — audit primacy is contractual (CLAUDE.md: "if it's not
    recorded, it didn't happen"). The standalone MCP and web-composer
    dispatch sites both implement the same try/finally shape used by
    ``AuditedLLMClient.chat_completion`` to make "audit fires before
    return" structurally enforceable.
    """

    def record(self, invocation: ComposerToolInvocation) -> None:
        """Persist one invocation. Called once per dispatch."""
        ...

    def resolve_session(self, session_id: str) -> None:
        """Hint that the session_id is now resolved.

        Recorders that buffer pre-resolution records use this hook to
        flush. In-memory recorders (e.g.
        :class:`~elspeth.web.composer.audit.BufferingRecorder`) should
        implement this as a no-op — there is nothing to flush.

        Lifting this onto the Protocol (rather than calling
        ``isinstance(recorder, JsonlEventRecorder)`` from the server)
        keeps the abstraction whole: any future recorder
        implementation can opt into pre-resolution behaviour without
        the dispatch site needing to know.
        """
        ...
