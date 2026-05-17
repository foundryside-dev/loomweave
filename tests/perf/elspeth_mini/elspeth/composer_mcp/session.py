"""Session persistence for the composer MCP server.

Sessions are stored as JSON files in the scratch directory.
Each file contains the serialized CompositionState (via to_dict/from_dict
round-trip) plus session metadata.

Layer: L3 (application). Imports from L3 (web.composer.state).
"""

from __future__ import annotations

import importlib.util
import json
import os
import re
import tempfile
import threading
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

if importlib.util.find_spec("fcntl") is None:  # pragma: no cover - Windows fallback
    fcntl_module: Any = None
else:
    import fcntl as fcntl_module

from elspeth.web.composer.state import CompositionState, PipelineMetadata

# Tier-3 boundary: session_id arrives as an LLM-controlled MCP argument.
# The valid shape is the output of ``new_session`` — ``uuid.uuid4().hex[:12]``
# — a 12-character lowercase hex string. Anything else is either a client
# bug or a path-traversal attempt; either way, reject at the boundary
# rather than coerce (silent coercion would re-point the agent's intended
# session to a different file, a meaning-changing operation).
_SESSION_ID_RE = re.compile(r"^[a-f0-9]{12}$")

# Process-local fast path for thread-level serialization. See
# ``SessionManager._locked_session`` for the cross-process lock that pairs
# with this registry when ``fcntl`` is available.
_SESSION_LOCKS: dict[str, threading.Lock] = {}
_SESSION_LOCKS_REGISTRY_MUTEX = threading.Lock()


class InvalidSessionIdError(ValueError):
    """Raised when a session_id does not match the allowed shape.

    Subclasses ``ValueError`` for compatibility with callers that treat
    invalid identifier syntax as a value-domain failure. The MCP tool
    boundary converts this specific exception to ``ToolArgumentError``;
    generic ``ValueError`` escaping dispatch remains a plugin crash.
    """

    def __init__(self, session_id: str) -> None:
        # The message echoes only the caller-supplied value — never a
        # server-side filesystem path — so no information is leaked back
        # to the LLM that it didn't already provide.
        super().__init__(f"Invalid session_id: {session_id!r}")
        self.session_id = session_id


class CorruptSessionFileError(ValueError):
    """Raised when a canonical session file cannot be parsed as a session."""

    def __init__(self, session_id: str, reason: str) -> None:
        super().__init__(f"Corrupt session file for {session_id}: {reason}")
        self.session_id = session_id
        self.reason = reason


class StaleSessionVersionError(ValueError):
    """Raised when a save would overwrite a newer on-disk session version."""

    def __init__(self, session_id: str, *, incoming_version: int, on_disk_version: int) -> None:
        super().__init__(
            f"Refusing stale save for {session_id}: incoming version {incoming_version} is older than on-disk version {on_disk_version}."
        )
        self.session_id = session_id
        self.incoming_version = incoming_version
        self.on_disk_version = on_disk_version


def _validate_session_id(session_id: str) -> None:
    """Enforce the session_id shape at the filesystem boundary.

    Called from ``_session_path`` so every read/write/delete path inherits
    the guard automatically — a future method that calls ``_session_path``
    cannot accidentally bypass validation.

    The MCP tool schema declares ``session_id`` as ``"type": "string"``,
    so non-str inputs are a contract violation, not an attack. If one
    slips through, ``re.Pattern.fullmatch`` raises ``TypeError`` which
    the server's top-level handler converts to a clean tool error —
    equivalent security outcome, one fewer defensive branch.
    """
    if not _SESSION_ID_RE.fullmatch(session_id):
        raise InvalidSessionIdError(session_id)


def _session_lock(session_id: str) -> threading.Lock:
    """Return the process-local mutex guarding one canonical session ID."""
    if session_id in _SESSION_LOCKS:
        return _SESSION_LOCKS[session_id]
    with _SESSION_LOCKS_REGISTRY_MUTEX:
        if session_id not in _SESSION_LOCKS:
            _SESSION_LOCKS[session_id] = threading.Lock()
        return _SESSION_LOCKS[session_id]


class SessionNotFoundError(Exception):
    """Raised when a session ID does not correspond to a saved file."""

    def __init__(self, session_id: str) -> None:
        super().__init__(f"Session not found: {session_id}")
        self.session_id = session_id


class SessionManager:
    """Manages CompositionState sessions as JSON files on disk.

    Each session is a single JSON file: ``{scratch_dir}/{session_id}.json``.
    The file contains the full CompositionState serialized via to_dict().
    """

    def __init__(self, scratch_dir: Path) -> None:
        self._dir = scratch_dir

    def new_session(self, *, name: str = "Untitled Pipeline") -> tuple[str, CompositionState]:
        """Create a new empty session. Does not persist until save() is called."""
        session_id = uuid.uuid4().hex[:12]
        state = CompositionState(
            source=None,
            nodes=(),
            edges=(),
            outputs=(),
            metadata=PipelineMetadata(name=name),
            version=1,
        )
        return session_id, state

    def save(self, session_id: str, state: CompositionState) -> Path:
        """Persist session state to disk. Creates scratch dir if needed."""
        self._dir.mkdir(parents=True, exist_ok=True)
        path = self._session_path(session_id)
        data = state.to_dict()
        serialized = json.dumps(data, indent=2, sort_keys=True)
        with self._locked_session(session_id):
            if path.exists():
                on_disk = self._read_session_state(path, session_id)
                if state.version < on_disk.version:
                    raise StaleSessionVersionError(
                        session_id,
                        incoming_version=state.version,
                        on_disk_version=on_disk.version,
                    )
            self._atomic_write(path, serialized)
        return path

    def load(self, session_id: str) -> CompositionState:
        """Load session state from disk."""
        path = self._session_path(session_id)
        if not path.exists():
            raise SessionNotFoundError(session_id)
        try:
            return self._read_session_state(path, session_id)
        except FileNotFoundError as exc:
            raise SessionNotFoundError(session_id) from exc

    def delete(self, session_id: str) -> None:
        """Delete a saved session and its audit events sidecar.

        The audit events sidecar (``{scratch}/{session_id}.events.jsonl``)
        is unlinked alongside the canonical session JSON file so the two
        live and die together. ``missing_ok=True`` on the sidecar:
        sessions that produced zero recordable events would have no
        sidecar to begin with, and the absence of a sidecar is not a
        deletion failure.
        """
        path = self._session_path(session_id)
        if not path.exists():
            raise SessionNotFoundError(session_id)
        # Lazy import to avoid a circular import at module load. ``audit``
        # imports nothing from ``session``, but ``server.py`` (the only
        # caller hub) imports both modules — keeping the import inside
        # the function body avoids any future circular-dep regression.
        from elspeth.composer_mcp.audit import events_sidecar_path

        events_path = events_sidecar_path(self._dir, session_id)
        try:
            with self._locked_session(session_id):
                if not path.exists():
                    raise SessionNotFoundError(session_id)
                path.unlink()
                events_path.unlink(missing_ok=True)
        except FileNotFoundError as exc:
            raise SessionNotFoundError(session_id) from exc

    def list_sessions(self) -> list[dict[str, Any]]:
        """List all saved sessions with ID, name, and version."""
        if not self._dir.exists():
            return []
        sessions: list[dict[str, Any]] = []
        for path in sorted(self._dir.glob("*.json")):
            session_id = path.stem
            if not _SESSION_ID_RE.fullmatch(session_id):
                continue
            found, state = self._read_session_state_for_listing(path, session_id)
            if not found or state is None:
                continue
            sessions.append(
                {
                    "session_id": session_id,
                    "name": state.metadata.name,
                    "version": state.version,
                }
            )
        return sessions

    def _session_path(self, session_id: str) -> Path:
        # Chokepoint guard — every filesystem-touching method (save, load,
        # delete) routes here, so validating once covers all three.
        _validate_session_id(session_id)
        return self._dir / f"{session_id}.json"

    def _lock_path(self, session_id: str) -> Path:
        _validate_session_id(session_id)
        return self._dir / f".{session_id}.lock"

    @contextmanager
    def _locked_session(self, session_id: str) -> Iterator[None]:
        """Serialize version-check + replace across threads and processes."""
        with _session_lock(session_id):
            if fcntl_module is None:
                yield
                return
            lock_path = self._lock_path(session_id)
            with lock_path.open("a+", encoding="utf-8") as lock_file:
                fcntl_module.flock(lock_file.fileno(), fcntl_module.LOCK_EX)
                try:
                    yield
                finally:
                    fcntl_module.flock(lock_file.fileno(), fcntl_module.LOCK_UN)

    def _read_session_state_for_listing(self, path: Path, session_id: str) -> tuple[bool, CompositionState | None]:
        """Load one session for list_sessions without failing the whole listing."""
        try:
            return True, self._read_session_state(path, session_id)
        except (CorruptSessionFileError, FileNotFoundError):
            return False, None

    def _read_session_state(self, path: Path, session_id: str) -> CompositionState:
        """Parse one canonical session file into a validated state snapshot."""
        try:
            raw = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            raise
        except OSError as exc:
            raise CorruptSessionFileError(session_id, f"could not read file: {exc}") from exc
        try:
            data = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise CorruptSessionFileError(session_id, f"invalid JSON: {exc.msg}") from exc
        if type(data) is not dict:
            raise CorruptSessionFileError(session_id, "top-level JSON value must be an object")
        try:
            return CompositionState.from_dict(data)
        except (KeyError, TypeError, ValueError) as exc:
            raise CorruptSessionFileError(session_id, f"invalid session payload: {exc}") from exc

    def _atomic_write(self, path: Path, serialized: str) -> None:
        """Write to a sibling tempfile and replace the canonical file atomically."""
        tmp_fd, tmp_name = tempfile.mkstemp(
            dir=path.parent,
            prefix=f".{path.name}.",
            suffix=".tmp",
            text=True,
        )
        tmp_path = Path(tmp_name)
        try:
            with os.fdopen(tmp_fd, "w", encoding="utf-8") as tmp_file:
                tmp_file.write(serialized)
                tmp_file.flush()
                os.fsync(tmp_file.fileno())
            tmp_path.replace(path)
        except Exception:
            tmp_path.unlink(missing_ok=True)
            raise
