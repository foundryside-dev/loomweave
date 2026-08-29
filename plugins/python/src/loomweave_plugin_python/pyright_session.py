"""Pyright-backed call / reference resolution for one plugin process.

Coverage reason vocabulary (``FacetCoverage.reason``; ADR-057). ``collateral``
is decided by WHICH catch-site caught the failure, never by message text:

- ``syntax_error`` / ``reference_site_cap`` -- content-determined
  (``transient=False``); a re-run hits them again.
- ``pyright_timeout`` -- this file's own per-query or file budget expired
  while pyright stayed alive. Self-inflicted (``collateral=False``). A single
  timeout does not restart: the process is slow, not dead. A streak of
  ``MAX_CONSECUTIVE_TIMEOUT_FILES`` consecutive files whose calls pass timed
  out is read as a wedged-but-alive process and restarted once, charged to
  the run-level ``MAX_PYRIGHT_RESTARTS_PER_RUN`` budget; the streak's files
  keep their self-inflicted claim.
- ``pyright_transport_failure`` -- pyright died WHILE THIS FILE'S REQUEST was
  in flight. Self-inflicted. Restarted immediately, before the result is
  returned, so the next file arrives at a live process; the restart is
  file-attributed and does not spend ``MAX_PYRIGHT_RESTARTS_PER_RUN``.
- ``pyright_local_read_error`` -- an ``OSError`` reading an unrelated target
  file mid-pass while pyright is confirmed alive. This file's evidence gap,
  but not a pyright-health event: no restart.
- ``pyright_restarting`` -- pyright was already dead when this file arrived
  (it died after answering the previous file). Collateral; the restart spends
  the run-level ``MAX_PYRIGHT_RESTARTS_PER_RUN`` budget.
- ``pyright_spawn_failed`` -- a transient spawn deferral (resource pressure)
  left this file without a process. Collateral.
- ``pyright_unavailable`` / ``pyright_poisoned`` /
  ``pyright_restart_cap_exceeded`` -- the run is disabled; every later file is
  collateral. ``PyrightRunState.disabled_reason`` is the single source of
  truth for which of these applies.
"""

from __future__ import annotations

import ast
import builtins
import contextlib
import ctypes
import ctypes.util
import errno
import json
import math
import os
import select
import shutil
import signal
import subprocess
import sys
import threading
import time
import tokenize
from dataclasses import dataclass
from io import StringIO
from pathlib import Path
from typing import IO, TYPE_CHECKING, Any, Literal, Self
from urllib.parse import unquote, urlparse

from loomweave_plugin_python import __version__
from loomweave_plugin_python.call_resolver import (
    CallResolutionResult,
    CallsRawEdge,
    FacetCoverage,
    Finding,
    UnresolvedCallSite,
)
from loomweave_plugin_python.entity_id import entity_id
from loomweave_plugin_python.extractor import module_dotted_name
from loomweave_plugin_python.qualname import reconstruct_qualname
from loomweave_plugin_python.reference_resolver import (
    ReferenceResolutionResult,
    ReferenceSite,
    ReferencesRawEdge,
)

FINDING_PYRIGHT_RESTART = "LMWV-PY-PYRIGHT-RESTART"
FINDING_PYRIGHT_POISON_FRAME = "LMWV-PY-PYRIGHT-POISON-FRAME"
FINDING_PYRIGHT_WEDGED_RESTART = "LMWV-PY-PYRIGHT-WEDGED-RESTART"
FINDING_PYRIGHT_TOTAL_RESTART_CAP_EXCEEDED = "LMWV-PY-PYRIGHT-TOTAL-RESTART-CAP"
FINDING_PYRIGHT_INIT_TIMEOUT = "LMWV-PY-PYRIGHT-INIT-TIMEOUT"
FINDING_PYRIGHT_UNAVAILABLE = "LMWV-PY-PYRIGHT-UNAVAILABLE"
FINDING_PYRIGHT_INSTALL_FAILURE = "LMWV-PY-PYRIGHT-INSTALL-FAILURE"
FINDING_PYRIGHT_SPAWN_DEFERRED = "LMWV-PY-PYRIGHT-SPAWN-DEFERRED"
FINDING_PYRIGHT_RESOURCE_EXHAUSTED = "LMWV-PY-PYRIGHT-RESOURCE-EXHAUSTED"
FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT = "LMWV-PY-CALL-RESOLUTION-TIMEOUT"
FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT = "LMWV-PY-REFERENCE-RESOLUTION-TIMEOUT"
FINDING_PYRIGHT_REFERENCE_SITE_CAP = "LMWV-PY-REFERENCE-SITE-CAP"


@dataclass
class PyrightRunState:
    """Run-wide pyright health budget, shared across session recycles.

    A ``PyrightSession`` is recycled every ``MAX_FILES_PER_PYRIGHT_SESSION``
    files to bound memory growth. Without a shared budget the 3-restart cap
    resets at every recycle boundary, letting a crash-looping pyright silently
    consume ``ceil(N/25) * 3`` restarts instead of 3 for an entire analysis
    run. Pass the same ``PyrightRunState`` instance to every successive
    ``PyrightSession`` so the budget is enforced across the full run.

    ``consecutive_spawn_deferrals`` tracks transient (resource-pressure) spawn
    failures separately from the ``restart_count`` crash budget: it is reset to
    zero on every successful spawn, so intermittent pressure never poisons the
    run, while a sustained run of deferrals still terminates pyright once it
    exceeds ``MAX_CONSECUTIVE_SPAWN_DEFERRALS``.

    Restart attribution (clarion-7fc41105ea, ADR-057):

    - ``restart_count`` counts only restarts NOT attributable to a file --
      pyright found dead on arrival, i.e. it died after answering the previous
      file. Guarded by ``MAX_PYRIGHT_RESTARTS_PER_RUN``; exceeding it poisons
      the run.
    - ``file_attributed_restart_count`` counts immediate restarts issued right
      after pyright died during THIS file's own request. These do not spend
      the run-level cap: the troublemaker owns its hole and the next file must
      arrive at a live process. Jointly with ``restart_count`` they are bounded
      by ``MAX_TOTAL_PYRIGHT_RESTARTS_PER_RUN`` and by the cumulative init
      latency budget ``MAX_PYRIGHT_RESTART_LATENCY_BUDGET_MS``.
    - ``file_attributed_respawn_failure_count`` counts immediate restarts whose
      respawn itself failed; the spawn path's own triage decides what happens.
    - ``ceiling_deferred_restart_count`` / ``restart_already_charged_to_file``
      / ``restart_charged_to_path``: when the crashing file has too little
      REAL wall-clock headroom left under the host's per-file watchdog ceiling
      for a respawn (or a headroom-bounded respawn handshake ran out of it),
      the respawn is deferred to the next file's ``_ensure_process``. The
      one-shot flag makes that next file respawn silently instead of being
      mis-charged as collateral for a death it did not cause; the charged
      path lets the SAME file's later facet (references after calls) in the
      same window recognise the dead process as its own doing and stay
      self-inflicted rather than respawn with no headroom.
    - ``pyright_init_latency_total_ms`` is the cumulative wall-clock spent in
      spawn + initialize handshakes for the run.
    - ``consecutive_timeout_files`` / ``wedged_restart_count``: the
      wedged-but-alive breaker. A pyright that hangs on every query still
      answers ``poll()`` as alive, so no death is ever observed and no restart
      would ever be attempted while every remaining file times out. The
      counter is incremented once per ``resolve_calls`` whose coverage lands
      on ``pyright_timeout`` (the calls pass is a file's first facet; the
      references pass does not participate), reset whenever a calls pass
      completes or a process is successfully spawned. Reaching
      ``MAX_CONSECUTIVE_TIMEOUT_FILES`` restarts pyright once, charged to
      ``restart_count`` so a persistent wedge is bounded by the run-level cap
      like any other crash loop.
    - ``disabled_reason`` is the ONLY authority on why the run is disabled.
      Every site that sets ``disabled = True`` sets it in the same statement
      and never overwrites an already-set value.
    """

    restart_count: int = 0
    disabled: bool = False
    consecutive_spawn_deferrals: int = 0
    disabled_reason: str | None = None
    file_attributed_restart_count: int = 0
    file_attributed_respawn_failure_count: int = 0
    ceiling_deferred_restart_count: int = 0
    restart_already_charged_to_file: bool = False
    restart_charged_to_path: str | None = None
    pyright_init_latency_total_ms: int = 0
    consecutive_timeout_files: int = 0
    wedged_restart_count: int = 0


MAX_UNRESOLVED_CALLEE_EXPR_BYTES = 512
MAX_PYRIGHT_RESTARTS_PER_RUN = 3
# Safety budget on ALL restarts (run-level + file-attributed) so a corpus whose
# files each independently crash pyright cannot spend hours re-initialising it.
# Count OR cumulative init latency trips it: 25 restarts at the 30s init
# timeout would otherwise be ~12.5 minutes of pure restart overhead; the 4
# minute latency budget caps that at ~2.7x today's ~90s worst case (ADR-057).
MAX_TOTAL_PYRIGHT_RESTARTS_PER_RUN = 25
MAX_PYRIGHT_RESTART_LATENCY_BUDGET_MS = 240_000
# A spawn that fails with one of these errnos is a *transient* resource-pressure
# condition (the host is momentarily out of process slots / memory), not a broken
# install. EAGAIN in particular is what a busy workstation returns from fork(2)
# when the per-UID RLIMIT_NPROC is hit. These are deferred-and-retried rather
# than treated as a permanent install failure.
_TRANSIENT_SPAWN_ERRNOS = frozenset({errno.EAGAIN, errno.ENOMEM, errno.EMFILE, errno.ENFILE})
# Upper bound on *consecutive* transient spawn deferrals before pyright is
# disabled for the run. Reset to zero on any successful spawn, so this only
# fires under sustained pressure, never on an intermittent blip. A failed fork
# costs microseconds, so retrying once per file across a large run is cheap.
MAX_CONSECUTIVE_SPAWN_DEFERRALS = 50
# Consecutive files whose calls pass ended in ``pyright_timeout`` before the
# live-but-unresponsive process is presumed wedged and restarted once
# (ADR-057). One slow file is just a slow file; three in a row with no
# completion between them is a process that answers nothing.
MAX_CONSECUTIVE_TIMEOUT_FILES = 3
MAX_REFERENCE_SITES_PER_FILE = 2000
PYRIGHT_INIT_TIMEOUT_SECS = 30.0
# Per-LSP-request grant. The FIRST callHierarchy/definition query after
# ``didOpen`` on a large file makes pyright analyse the whole file before it
# answers; on 5k-line modules that warm-up alone exceeded 5 s, so one timeout
# aborted the calls pass with almost no evidence (clarion-5d83413c36) even
# though the file completed in ~11 s total once the first answer landed.
# ADR-035 —
# Basis: elspeth 2026-08-29, guided.py / pipeline_planner.py /
#   guided_chat_atomic.py: 5 s → 28/3/20 calls edges (degraded); 120 s →
#   633/461/153 edges (complete) in 11.2 / 12.6 / 10.3 s wall.
# Override surface: none (internal); ``PyrightSession(call_timeout_secs=...)``
#   for tests.
# Retune trigger: a ``pyright_timeout`` whose single request exceeded the
#   grant on a file that later completes with a larger grant.
# Coupling: the effective grant is ``min(this, remaining file budget)``
#   (``_budgeted_timeout``), so ``PYRIGHT_FILE_TIMEOUT_*`` and the host's
#   ``DEFAULT_PLUGIN_FILE_TIMEOUT`` (120 s) still bound a wedged server; the
#   ADR-057 wedge breaker counts files, not requests, and is unaffected.
PYRIGHT_CALL_TIMEOUT_SECS = 30.0
# Per-file wall-clock budget, shared by the calls and references passes for
# one file: ``base + per_function * n_functions``, capped
# (clarion-7f527d3d32). A flat budget starved large, heavily-typed files
# (numpy/torch-vectorised ML code) and pinned them as degraded; scaling with
# the function count buys a big file the time its size demands without
# handing a small one a budget it will never use.
PYRIGHT_FILE_TIMEOUT_BASE_SECS = 10.0
PYRIGHT_FILE_TIMEOUT_PER_FUNCTION_SECS = 0.25
# The host watchdog (``DEFAULT_PLUGIN_FILE_TIMEOUT`` in
# crates/loomweave-cli/src/analyze.rs) kills an ``analyze_file`` call at 120s.
# Stay well under it so the plugin always hands back the partial evidence it
# resolved before the deadline instead of losing it with the killed call.
PYRIGHT_FILE_TIMEOUT_CAP_SECS = 90.0
# Default of the host's per-file watchdog (``DEFAULT_PLUGIN_FILE_TIMEOUT`` in
# crates/loomweave-cli/src/analyze.rs). The host honours
# ``LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS`` and the plugin subprocess inherits its
# environment, so ``resolve_host_file_watchdog_secs()`` reads the SAME override: a
# session that assumed 120s against a 30s host deadline would respawn pyright
# in-process past the real deadline and get the whole plugin call killed.
# An immediate restart extends the crashing file's deadline by the respawn's
# latency, but never past the ceiling ``first touch + watchdog -
# FILE_DEADLINE_TERMINAL_SAFETY_MARGIN_SECS``: the margin covers response
# marshaling plus the AST-extraction latency that runs before ``resolve_calls``
# is entered (server.py), which this session cannot see. A watchdog too short
# for the full margin keeps at least half of itself as the window.
#
# Whether a respawn is attempted is decided on the REAL headroom
# ``ceiling - now`` at the moment pyright dies -- not on the file's deadline,
# which is anchored at file start and says nothing about how late in the
# window the crash happened. Below ``MIN_RESPAWN_HEADROOM_SECS`` the respawn
# is deferred to the next file; otherwise the initialize handshake is bounded
# to the headroom, so a hung respawn times out inside the window and is
# deferred instead of the host's watchdog killing the whole plugin call.
HOST_FILE_WATCHDOG_SECS = 120.0
HOST_FILE_WATCHDOG_ENV = "LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS"
FILE_DEADLINE_TERMINAL_SAFETY_MARGIN_SECS = 15.0
# A respawn with less real headroom than this is pointless: pyright's
# initialize handshake rarely completes faster on a machine that just lost it.
MIN_RESPAWN_HEADROOM_SECS = 5.0


def resolve_host_file_watchdog_secs(environ: Mapping[str, str] | None = None) -> float:
    """The host's per-file watchdog, as the plugin process can observe it.

    Mirrors ``plugin_file_timeout()`` in crates/loomweave-cli/src/analyze.rs:
    ``LOOMWEAVE_PLUGIN_FILE_TIMEOUT_MS`` when it parses as a positive integer,
    else the host's default. The host ignores an unparsable value the same way.
    """
    env = os.environ if environ is None else environ
    raw = env.get(HOST_FILE_WATCHDOG_ENV)
    if raw is None:
        return HOST_FILE_WATCHDOG_SECS
    try:
        millis = int(raw)
    except ValueError:
        return HOST_FILE_WATCHDOG_SECS
    if millis <= 0:
        return HOST_FILE_WATCHDOG_SECS
    return millis / 1000.0


STDERR_TAIL_LIMIT = 65536
PYRIGHT_EXCLUDE_PATTERNS = [
    "**/.weft/**",
    "**/.git/**",
    "**/.hg/**",
    "**/.svn/**",
    "**/.jj/**",
    "**/.venv/**",
    "**/__pycache__/**",
    "**/node_modules/**",
]
PROJECT_LOCAL_EXTERNAL_DIRS = {".weft", ".git", ".hg", ".svn", ".jj", ".venv", "node_modules"}
_IMPLICIT_MODULE_NAMES = {
    "__annotations__",
    "__builtins__",
    "__cached__",
    "__debug__",
    "__doc__",
    "__file__",
    "__loader__",
    "__name__",
    "__package__",
    "__path__",
    "__spec__",
}
_BUILTIN_NAMES = frozenset(dir(builtins)) - _IMPLICIT_MODULE_NAMES
_TYPE_PARAMETER_NODE_NAMES = frozenset({"ParamSpec", "TypeVar", "TypeVarTuple"})


if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence


class LspTimeoutError(TimeoutError):
    def __init__(self, method: str) -> None:
        super().__init__(f"{method} timed out")
        self.method = method


class LspTransportClosedError(RuntimeError):
    pass


@dataclass(frozen=True)
class _CallPassAbort:
    """Why the calls pass stopped early, carried with the evidence it kept.

    A token rather than the caught exception because ``LspTimeoutError`` is a
    ``TimeoutError`` and therefore an ``OSError``: dispatching on the
    exception type would misfile a timeout as a transport failure.
    """

    reason: Literal["pyright_timeout", "pyright_transport_failure", "pyright_local_read_error"]
    method: str | None
    message: str


_InFlightFailureReason = Literal["pyright_transport_failure", "pyright_local_read_error"]
_RestartOutcome = Literal["restarted", "deferred_to_next_file", "respawn_failed"]


@dataclass(frozen=True)
class _CallSite:
    line: int
    character: int
    end_line: int
    end_character: int
    callee_expr: str


@dataclass(frozen=True)
class _FunctionInfo:
    entity_id: str
    qualified_name: str
    name: str
    line: int
    character: int
    end_line: int
    end_character: int
    call_sites: tuple[_CallSite, ...]
    node: ast.FunctionDef | ast.AsyncFunctionDef


@dataclass(frozen=True)
class _EntityInfo:
    entity_id: str
    line: int
    character: int


@dataclass(frozen=True)
class _FunctionIndex:
    source: str
    line_starts: tuple[int, ...]
    parse_latency_ms: int
    module_id: str
    by_id: dict[str, _FunctionInfo]
    by_name_position: dict[tuple[int, int], _FunctionInfo]
    entity_by_name_position: dict[tuple[int, int], str]
    by_short_name: dict[str, str]
    dunder_call_by_class: dict[str, str]
    functions: tuple[_FunctionInfo, ...]
    entities: tuple[_EntityInfo, ...]
    tree: ast.Module
    parse_status: Literal["ok", "syntax_error"] = "ok"


@dataclass
class _ReferenceEdgeAccumulator:
    kind: Literal["references", "inherits_from", "decorates"]
    from_id: str
    to_id: str
    source_byte_start: int
    source_byte_end: int
    candidates: set[str]


# Site kind → emitted edge kind (clarion-43416be550). `name`/`annotation`
# sites keep producing `references`; the two relation kinds map onto the
# ontology kinds that were previously declared-but-dead for Python.
_EDGE_KIND_BY_SITE_KIND: dict[str, Literal["references", "inherits_from", "decorates"]] = {
    "name": "references",
    "annotation": "references",
    "base": "inherits_from",
    "decorator": "decorates",
}


class PyrightSession:
    def __init__(  # noqa: PLR0913 - knobs are tested lifecycle boundaries.
        self,
        project_root: str | Path,
        *,
        executable: str = "pyright-langserver",
        env: dict[str, str] | None = None,
        install_check: Callable[[str], bool] | None = None,
        init_timeout_secs: float = PYRIGHT_INIT_TIMEOUT_SECS,
        call_timeout_secs: float = PYRIGHT_CALL_TIMEOUT_SECS,
        file_timeout_base_secs: float = PYRIGHT_FILE_TIMEOUT_BASE_SECS,
        file_timeout_per_function_secs: float = PYRIGHT_FILE_TIMEOUT_PER_FUNCTION_SECS,
        file_timeout_cap_secs: float = PYRIGHT_FILE_TIMEOUT_CAP_SECS,
        max_restarts_per_run: int = MAX_PYRIGHT_RESTARTS_PER_RUN,
        max_total_restarts_per_run: int = MAX_TOTAL_PYRIGHT_RESTARTS_PER_RUN,
        restart_latency_budget_ms: int = MAX_PYRIGHT_RESTART_LATENCY_BUDGET_MS,
        max_reference_sites_per_file: int = MAX_REFERENCE_SITES_PER_FILE,
        max_consecutive_timeout_files: int = MAX_CONSECUTIVE_TIMEOUT_FILES,
        run_state: PyrightRunState | None = None,
        host_file_watchdog_secs: float | None = None,
    ) -> None:
        self.project_root = Path(project_root).resolve()
        # Resolved once per session from the host's env override (see
        # ``resolve_host_file_watchdog_secs``); an explicit value is for tests.
        self.host_file_watchdog_secs = (
            host_file_watchdog_secs
            if host_file_watchdog_secs is not None
            else resolve_host_file_watchdog_secs()
        )
        self.executable = executable
        self.env = env
        self.install_check = install_check
        self.init_timeout_secs = init_timeout_secs
        self.call_timeout_secs = call_timeout_secs
        self.file_timeout_base_secs = file_timeout_base_secs
        self.file_timeout_per_function_secs = file_timeout_per_function_secs
        self.file_timeout_cap_secs = file_timeout_cap_secs
        self.max_restarts_per_run = max_restarts_per_run
        self.max_total_restarts_per_run = max_total_restarts_per_run
        self.restart_latency_budget_ms = restart_latency_budget_ms
        self.max_reference_sites_per_file = max_reference_sites_per_file
        self.max_consecutive_timeout_files = max_consecutive_timeout_files
        # Run-wide health budget: shared across session recycles when the caller
        # passes an explicit ``run_state``; isolated (per-instance) otherwise,
        # which preserves the existing contract for code that constructs
        # ``PyrightSession`` directly without going through ``ServerState``.
        self._run_state = run_state if run_state is not None else PyrightRunState()
        self._process: subprocess.Popen[bytes] | None = None
        self._stderr_thread: threading.Thread | None = None
        self._stderr_tail = bytearray()
        self._next_id = 1
        self._findings: list[Finding] = []
        self._function_indexes: dict[Path, _FunctionIndex] = {}
        self._index_parse_latency_ms: list[int] = []
        self._file_deadlines: dict[Path, float] = {}
        # When each file was first touched by this session (its calls pass, or
        # its references pass when called alone): the anchor for the
        # host-watchdog ceiling any restart-driven deadline extension respects.
        self._file_started_at: dict[Path, float] = {}
        # Set by ``_handle_initialize_timeout`` when a respawn's handshake was
        # bounded to a crashing file's remaining headroom and ran out of it:
        # that is a deferral, not a broken install.
        self._bounded_init_timed_out = False
        # Set by ``_resolve_references_with_pyright`` when a per-site or
        # file-budget timeout skipped sites (clarion-3e517d4aff): the pass
        # still returns normally, but its coverage is degraded.
        self._reference_pass_timed_out = False
        # Likewise for a pyright transport failure mid-pass
        # (clarion-7f527d3d32). Deliberately flag-based like its sibling above,
        # rather than the ``_CallPassAbort`` token the calls pass returns: the
        # references loop already reports through flags.
        self._reference_pass_transport_failed = False
        self._reference_pass_failure_message = ""
        # And for an ``OSError`` on an unrelated target file while pyright is
        # confirmed alive: this file's gap, but no restart (ADR-057).
        self._reference_pass_local_read_error = False

    @property
    def run_state(self) -> PyrightRunState:
        return self._run_state

    def __enter__(self) -> Self:
        return self

    def __exit__(self, exc_type: object, exc: object, tb: object) -> None:
        _ = (exc_type, exc, tb)
        self.close()

    @property
    def stderr_thread_alive(self) -> bool:
        return self._stderr_thread is not None and self._stderr_thread.is_alive()

    def kill_for_test(self) -> None:
        if self._process is None or self._process.poll() is not None:
            return
        self._process.kill()
        self._process.wait(timeout=2)

    def close(self) -> None:
        process = self._process
        if process is not None and process.poll() is None:
            try:
                self._request("shutdown", {}, self.call_timeout_secs)
                self._notify("exit", {})
            except (LspTimeoutError, LspTransportClosedError, BrokenPipeError, OSError):
                process.kill()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=2)
        self._process = None
        if self._stderr_thread is not None:
            self._stderr_thread.join(timeout=2)

    def resolve_calls(
        self,
        file_path: str | Path,
        function_ids: Sequence[str],
    ) -> CallResolutionResult:
        path = Path(file_path).resolve()
        # The calls pass is a file's first facet: start its shared window
        # fresh so a stale deadline from an earlier visit is never reused.
        self._file_deadlines.pop(path, None)
        self._file_started_at[path] = self._now()
        index = self._function_index_for_path(path)
        if index.parse_status == "syntax_error":
            return CallResolutionResult(
                unresolved_call_sites_total=len(function_ids),
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
                coverage=FacetCoverage.degraded("syntax_error", transient=False),
            )
        requested = [
            index.by_id[function_id] for function_id in function_ids if function_id in index.by_id
        ]
        ast_call_sites_total = sum(len(function.call_sites) for function in requested)
        if not requested:
            return CallResolutionResult(
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
            )

        if self._own_deferred_restart_blocks(path) or not self._ensure_process():
            # No pyright: nothing was examined. Say so (clarion-3e517d4aff) --
            # returning only the site COUNT reads to the host as a completed
            # analysis of a call-free file.
            return CallResolutionResult(
                unresolved_call_sites_total=ast_call_sites_total,
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
                coverage=self._unavailable_coverage(),
            )

        deadline = self._deadline_for_file(path, len(index.functions))
        latency_started = time.perf_counter()
        coverage = FacetCoverage()
        try:
            edges, unresolved, unresolved_sites, abort = self._resolve_with_pyright(
                path,
                index,
                requested,
                deadline,
            )
        # These two arms remain reachable only from the ``didOpen`` notify
        # that precedes the per-function loop: nothing was visited yet, so
        # zero evidence is exact. Aborts inside the loop come back as a token
        # with the evidence resolved before them (clarion-7f527d3d32).
        except LspTimeoutError as exc:
            self._record_finding(
                FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT,
                f"pyright query timed out: {exc.method}",
                method=exc.method,
            )
            edges = []
            unresolved = ast_call_sites_total
            unresolved_sites = []
            coverage = FacetCoverage.degraded("pyright_timeout", transient=True)
        except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
            # The document never opened: pyright was dead on arrival, which
            # is the previous file's death, not this one's (ADR-057).
            self._record_arrival_death(str(exc))
            edges = []
            unresolved = ast_call_sites_total
            unresolved_sites = []
            coverage = FacetCoverage.degraded("pyright_restarting", transient=True, collateral=True)
        else:
            if abort is not None:
                if abort.reason == "pyright_timeout":
                    self._record_finding(
                        FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT,
                        f"pyright query timed out: {abort.method}",
                        method=abort.method,
                    )
                elif abort.reason == "pyright_transport_failure":
                    # ``_resolve_with_pyright`` has already sent didClose on
                    # the dead pipe, so respawning here cannot misorder it.
                    self._record_file_attributed_restart(path, abort.message)
                coverage = FacetCoverage.degraded(abort.reason, transient=True)
        latency_ms = max(1, math.ceil((time.perf_counter() - latency_started) * 1000))
        self._note_calls_facet_outcome(path, coverage)

        return CallResolutionResult(
            edges=edges,
            unresolved_call_sites_total=unresolved,
            unresolved_call_sites=unresolved_sites,
            pyright_query_latency_ms=[latency_ms],
            pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
            findings=self._pop_findings(),
            coverage=coverage,
        )

    def _unavailable_coverage(self) -> FacetCoverage:
        """The coverage for a pass that never got a process.

        Reads ``disabled_reason`` directly -- never re-derived from a counter
        comparison, which went stale the moment a second cap existed. Whatever
        kept the process away (disabled run, deferred spawn) predates this
        file and is collateral -- with one exception: an armed
        ``restart_already_charged_to_file`` here means
        ``_own_deferred_restart_blocks`` refused to respawn because THIS
        file's own earlier facet killed pyright and consumed the window, which
        stays self-inflicted (ADR-057).
        """
        state = self._run_state
        if state.disabled:
            return FacetCoverage.degraded(
                state.disabled_reason or "pyright_unavailable",
                transient=True,
                collateral=True,
            )
        if state.restart_already_charged_to_file:
            return FacetCoverage.degraded("pyright_transport_failure", transient=True)
        return FacetCoverage.degraded("pyright_spawn_failed", transient=True, collateral=True)

    def _own_deferred_restart_blocks(self, path: Path) -> bool:
        """True when ``path``'s own crash deferred a restart it still cannot afford.

        The deferral happened in an earlier facet of this same file (calls,
        then references) inside one shared watchdog window. Respawning now
        would spend the headroom that deferral already found missing -- and
        a spawn bounded only by ``init_timeout_secs`` could outrun the host's
        real deadline and get the whole plugin call killed. Leave the flag
        armed for the NEXT file, which starts a fresh window.
        """
        state = self._run_state
        if state.disabled or not state.restart_already_charged_to_file:
            return False
        if state.restart_charged_to_path != str(path):
            return False
        return self._respawn_headroom_secs(path) < MIN_RESPAWN_HEADROOM_SECS

    def resolve_references(
        self,
        file_path: str | Path,
        sites: Sequence[ReferenceSite],
    ) -> ReferenceResolutionResult:
        path = Path(file_path).resolve()
        self._file_started_at.setdefault(path, self._now())
        try:
            return self._resolve_references_for_file(path, sites)
        finally:
            # References is the last facet touched for a file: the shared
            # deadline and its anchor end here on every exit path.
            self._file_deadlines.pop(path, None)
            self._file_started_at.pop(path, None)

    def _resolve_references_for_file(
        self,
        path: Path,
        sites: Sequence[ReferenceSite],
    ) -> ReferenceResolutionResult:
        index = self._function_index_for_path(path)
        reference_sites_total = len(sites)
        if index.parse_status == "syntax_error":
            return ReferenceResolutionResult(
                reference_sites_total=reference_sites_total,
                unresolved_reference_sites_total=reference_sites_total,
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
                coverage=FacetCoverage.degraded("syntax_error", transient=False),
            )
        if not sites:
            return ReferenceResolutionResult(
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
            )
        if reference_sites_total > self.max_reference_sites_per_file:
            self._record_finding(
                FINDING_PYRIGHT_REFERENCE_SITE_CAP,
                "reference site cap exceeded; skipping reference resolution for file",
                reference_sites_total=reference_sites_total,
                max_reference_sites_per_file=self.max_reference_sites_per_file,
            )
            return ReferenceResolutionResult(
                reference_sites_total=reference_sites_total,
                references_skipped_cap_total=reference_sites_total,
                unresolved_reference_sites_total=reference_sites_total,
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
                coverage=FacetCoverage.degraded("reference_site_cap", transient=False),
            )
        if self._own_deferred_restart_blocks(path) or not self._ensure_process():
            return ReferenceResolutionResult(
                reference_sites_total=reference_sites_total,
                unresolved_reference_sites_total=reference_sites_total,
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
                coverage=self._unavailable_coverage(),
            )

        deadline = self._deadline_for_file(path, len(index.functions))
        latency_started = time.perf_counter()
        coverage = FacetCoverage()
        self._reference_pass_timed_out = False
        self._reference_pass_transport_failed = False
        self._reference_pass_local_read_error = False
        try:
            edges, resolved, skipped_external, unresolved = self._resolve_references_with_pyright(
                path,
                index,
                sites,
                deadline,
            )
            if self._reference_pass_transport_failed:
                coverage = FacetCoverage.degraded("pyright_transport_failure", transient=True)
                # Deliberately here, not in the per-site except: that except
                # sits inside the try whose ``finally`` sends didClose, and a
                # respawn from there would send didClose to a fresh process
                # for a URI it never opened.
                self._record_file_attributed_restart(path, self._reference_pass_failure_message)
            elif self._reference_pass_local_read_error:
                coverage = FacetCoverage.degraded("pyright_local_read_error", transient=True)
            elif self._reference_pass_timed_out:
                coverage = FacetCoverage.degraded("pyright_timeout", transient=True)
        # Both arms below are reachable only from the ``didOpen`` notify that
        # precedes the per-site loop: every timeout or transport failure inside
        # the loop is caught per site (evidence kept, flags set above). Keep
        # them as the net for "pyright died before the document even opened",
        # where zero evidence is exact.
        except LspTimeoutError as exc:
            self._record_finding(
                FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT,
                f"pyright reference query timed out: {exc.method}",
                method=exc.method,
            )
            edges = []
            resolved = 0
            skipped_external = 0
            unresolved = reference_sites_total
            coverage = FacetCoverage.degraded("pyright_timeout", transient=True)
        except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
            self._record_arrival_death(str(exc))
            edges = []
            resolved = 0
            skipped_external = 0
            unresolved = reference_sites_total
            coverage = FacetCoverage.degraded("pyright_restarting", transient=True, collateral=True)
        latency_ms = max(1, math.ceil((time.perf_counter() - latency_started) * 1000))

        return ReferenceResolutionResult(
            edges=edges,
            reference_sites_total=reference_sites_total,
            references_resolved_total=resolved,
            references_skipped_external_total=skipped_external,
            unresolved_reference_sites_total=unresolved,
            pyright_query_latency_ms=[latency_ms],
            pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
            findings=self._pop_findings(),
            coverage=coverage,
        )

    def _resolve_with_pyright(
        self,
        path: Path,
        index: _FunctionIndex,
        functions: Sequence[_FunctionInfo],
        deadline: float,
    ) -> tuple[list[CallsRawEdge], int, list[UnresolvedCallSite], _CallPassAbort | None]:
        uri = path.as_uri()
        self._notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": "python",
                    "version": 1,
                    "text": index.source,
                },
            },
        )
        try:
            edges: list[CallsRawEdge] = []
            unresolved_total = 0
            unresolved_sites: list[UnresolvedCallSite] = []
            abort: _CallPassAbort | None = None
            remaining_start = len(functions)
            for position, function in enumerate(functions):
                try:
                    function_edges, resolved_ranges = self._resolve_function_with_pyright(
                        uri,
                        index,
                        function,
                        deadline,
                    )
                except LspTimeoutError as exc:
                    abort = _CallPassAbort("pyright_timeout", exc.method, str(exc))
                    remaining_start = position
                    break
                except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
                    abort = _CallPassAbort(self._in_flight_failure_reason(exc), None, str(exc))
                    remaining_start = position
                    break
                edges.extend(function_edges)
                unresolved_total += _unresolved_call_site_total_for_function(
                    function,
                    resolved_ranges,
                )
                unresolved_sites.extend(
                    _unresolved_call_sites_for_function(index, function, resolved_ranges),
                )
            if abort is not None:
                # INVARIANT: this fallback is only correct because a function is
                # atomic with respect to ``edges`` -- its edges land in one
                # ``extend`` after ``_resolve_function_with_pyright`` returned,
                # i.e. after its whole outgoingCalls item sub-loop finished
                # without raising. So a function either already contributed
                # edges (before ``remaining_start``) or contributed none (this
                # range). If a future edit inserts an LSP-touching call between
                # the ``extend`` and the accounting above, ``remaining_start =
                # position`` double-counts that function (edges kept AND all
                # its sites unresolved); track "functions with edges appended"
                # explicitly instead of relying on ``position`` if you add one.
                for function in functions[remaining_start:]:
                    unresolved_total += _unresolved_call_site_total_for_function(function, set())
                    unresolved_sites.extend(
                        _unresolved_call_sites_for_function(index, function, set()),
                    )
            return edges, unresolved_total, unresolved_sites, abort
        finally:
            # The pipe may be the very thing that just failed: a notify over a
            # dead transport must not raise and discard the evidence above.
            with contextlib.suppress(LspTransportClosedError, BrokenPipeError, OSError):
                self._notify("textDocument/didClose", {"textDocument": {"uri": uri}})

    def _resolve_function_with_pyright(
        self,
        uri: str,
        index: _FunctionIndex,
        function: _FunctionInfo,
        deadline: float,
    ) -> tuple[list[CallsRawEdge], set[tuple[int, int, int, int]]]:
        """Resolve one function's call sites; the edges and the ranges they cover.

        Raises ``LspTimeoutError`` (per-query or file budget) or a transport
        error mid-way; the caller treats the function as wholly unresolved
        then -- nothing partial escapes from here.
        """
        self._ensure_file_budget(deadline)
        function_edges: list[CallsRawEdge] = []
        grouped: dict[tuple[int, int, int, int], set[str]] = {}
        prepared = self._request(
            "textDocument/prepareCallHierarchy",
            {
                "textDocument": {"uri": uri},
                "position": {"line": function.line, "character": function.character},
            },
            self._budgeted_timeout(deadline),
        )
        items = prepared if isinstance(prepared, list) else []
        for item in items:
            self._ensure_file_budget(deadline)
            outgoing = self._request(
                "callHierarchy/outgoingCalls",
                {"item": item},
                self._budgeted_timeout(deadline),
            )
            calls = outgoing if isinstance(outgoing, list) else []
            for call in calls:
                if not isinstance(call, dict):
                    continue
                to_id = self._target_id_from_call(call)
                if to_id is None:
                    continue
                from_ranges = call.get("fromRanges")
                if not isinstance(from_ranges, list):
                    continue
                for from_range in from_ranges:
                    key = _range_key(from_range)
                    if key is not None and _range_within_function(key, function):
                        grouped.setdefault(key, set()).add(to_id)

        for range_key, candidates in _ambiguous_dict_dispatches(index, function).items():
            grouped.setdefault(range_key, set()).update(candidates)
        for range_key, candidates in _dunder_call_dispatches(index, function).items():
            grouped.setdefault(range_key, set()).update(candidates)

        for range_key in sorted(grouped):
            candidate_ids = sorted(grouped[range_key])
            if not candidate_ids:
                continue
            start_line, start_character, end_line, end_character = range_key
            start_byte = _position_to_byte(index, start_line, start_character)
            end_byte = _position_to_byte(index, end_line, end_character)
            edge: CallsRawEdge = {
                "kind": "calls",
                "from_id": function.entity_id,
                "to_id": candidate_ids[0],
                "source_byte_start": start_byte,
                "source_byte_end": end_byte,
                "confidence": "resolved" if len(candidate_ids) == 1 else "ambiguous",
            }
            if len(candidate_ids) > 1:
                edge["properties"] = {"candidates": candidate_ids}
            function_edges.append(edge)

        return function_edges, set(grouped)

    def _resolve_references_with_pyright(
        self,
        path: Path,
        index: _FunctionIndex,
        sites: Sequence[ReferenceSite],
        deadline: float,
    ) -> tuple[list[ReferencesRawEdge], int, int, int]:
        uri = path.as_uri()
        self._notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": "python",
                    "version": 1,
                    "text": index.source,
                },
            },
        )
        try:
            accumulators: dict[tuple[str, str, str], _ReferenceEdgeAccumulator] = {}
            lookup_cache: dict[
                tuple[str, str, str, int, int, int, int], tuple[list[str], bool]
            ] = {}
            source_bytes = index.source.encode("utf-8")
            statically_external_names = _reference_fast_path_names(index.tree)
            resolved_total = 0
            skipped_external_total = 0
            unresolved_total = 0
            for site_index, site in enumerate(sites):
                if self._file_budget_expired(deadline):
                    unresolved_total += len(sites) - site_index
                    self._reference_pass_timed_out = True
                    self._record_finding(
                        FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT,
                        "pyright reference query timed out: analyze_file budget",
                        method="analyze_file budget",
                    )
                    break
                reference_root = _reference_root_name(site, source_bytes)
                if reference_root in statically_external_names:
                    unresolved_total += 1
                    skipped_external_total += 1
                    continue
                cache_key = _reference_lookup_cache_key(site, source_bytes)
                cached = lookup_cache.get(cache_key)
                if cached is None:
                    try:
                        candidate_ids, saw_external = self._lookup_reference_site(
                            uri,
                            site,
                            deadline,
                        )
                    except LspTimeoutError as exc:
                        self._reference_pass_timed_out = True
                        self._record_finding(
                            FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT,
                            f"pyright reference query timed out: {exc.method}",
                            method=exc.method,
                            line=site.line,
                            character=site.character,
                            source_byte_start=site.source_byte_start,
                            source_byte_end=site.source_byte_end,
                        )
                        unresolved_total += 1
                        continue
                    except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
                        # The pipe is gone: every remaining site is unresolved,
                        # but the sites resolved so far stay resolved
                        # (clarion-7f527d3d32). The restart itself happens in
                        # the caller, after this frame's didClose.
                        self._note_reference_pass_failure(exc)
                        unresolved_total += len(sites) - site_index
                        break
                    lookup_cache[cache_key] = (candidate_ids, saw_external)
                else:
                    candidate_ids, saw_external = cached
                if not candidate_ids:
                    unresolved_total += 1
                    if saw_external:
                        skipped_external_total += 1
                    continue
                resolved_total += 1
                _merge_reference_site(accumulators, site, candidate_ids)
            return (
                [
                    _reference_accumulator_to_edge(acc)
                    for acc in _sorted_reference_accumulators(accumulators)
                ],
                resolved_total,
                skipped_external_total,
                unresolved_total,
            )
        finally:
            with contextlib.suppress(LspTransportClosedError, BrokenPipeError, OSError):
                self._notify("textDocument/didClose", {"textDocument": {"uri": uri}})

    def _note_reference_pass_failure(self, exc: BaseException) -> None:
        if self._in_flight_failure_reason(exc) == "pyright_transport_failure":
            self._reference_pass_transport_failed = True
            self._reference_pass_failure_message = str(exc)
        else:
            self._reference_pass_local_read_error = True

    def _lookup_reference_site(
        self,
        uri: str,
        site: ReferenceSite,
        deadline: float,
    ) -> tuple[list[str], bool]:
        candidate_ids, saw_external = self._reference_target_ids(
            uri,
            site,
            deadline=deadline,
        )
        if not candidate_ids and site.kind == "annotation" and not saw_external:
            candidate_ids, fallback_external = self._reference_target_ids(
                uri,
                site,
                method="textDocument/typeDefinition",
                deadline=deadline,
            )
            saw_external = saw_external or fallback_external
        return _filter_relation_candidates(site, candidate_ids), saw_external

    def _reference_target_ids(
        self,
        uri: str,
        site: ReferenceSite,
        *,
        deadline: float,
        method: str = "textDocument/definition",
    ) -> tuple[list[str], bool]:
        result = self._request(
            method,
            {
                "textDocument": {"uri": uri},
                "position": {"line": site.line, "character": site.character},
            },
            self._budgeted_timeout(deadline),
        )
        # Relation sites (base/decorator) resolve to precise entities only:
        # the module-id coarse fallback would mint nonsense facts like
        # "class inherits_from module" for aliased bases.
        return self._target_ids_from_locations(
            result,
            precise_only=site.kind in ("base", "decorator"),
        )

    def _now(self) -> float:
        """The clock every deadline lives on (monotonic; overridable in tests)."""
        return time.monotonic()

    def _deadline_for_file(self, path: Path, n_functions: int) -> float:
        existing = self._file_deadlines.get(path)
        if existing is not None:
            return existing
        deadline = self._now() + self._file_timeout_for(n_functions)
        anchor = self._file_started_at.get(path)
        if anchor is not None:
            # A spawn paid before this point (first file, or a restart) has
            # already eaten into the host watchdog's window for this file.
            deadline = min(deadline, self._watchdog_ceiling_for(anchor))
        self._file_deadlines[path] = deadline
        return deadline

    def _watchdog_ceiling_for(self, anchor: float) -> float:
        watchdog = self.host_file_watchdog_secs
        window = max(watchdog - FILE_DEADLINE_TERMINAL_SAFETY_MARGIN_SECS, watchdog / 2.0)
        return anchor + window

    def _respawn_headroom_secs(self, path: Path) -> float:
        """Real wall-clock left under ``path``'s watchdog ceiling, right now.

        Deliberately NOT ``deadline >= ceiling``: both are anchored at file
        start, so that comparison reduces to ``budget >= window`` and never
        notices a crash 89s into a 90s budget with 16s of window left.
        """
        anchor = self._file_started_at.get(path)
        if anchor is None:
            return math.inf
        return self._watchdog_ceiling_for(anchor) - self._now()

    def _file_timeout_for(self, n_functions: int) -> float:
        budget = self.file_timeout_base_secs + self.file_timeout_per_function_secs * n_functions
        return min(budget, self.file_timeout_cap_secs)

    def _budgeted_timeout(self, deadline: float) -> float:
        remaining = deadline - self._now()
        if remaining <= 0:
            method = "analyze_file budget"
            raise LspTimeoutError(method)
        return min(self.call_timeout_secs, remaining)

    def _ensure_file_budget(self, deadline: float) -> None:
        if self._file_budget_expired(deadline):
            method = "analyze_file budget"
            raise LspTimeoutError(method)

    def _file_budget_expired(self, deadline: float) -> bool:
        return deadline - self._now() <= 0

    def _target_ids_from_locations(
        self,
        result: object,
        *,
        precise_only: bool = False,
    ) -> tuple[list[str], bool]:
        locations = result if isinstance(result, list) else [result]
        candidate_ids: set[str] = set()
        saw_external = False
        for location in locations:
            target_id, external = self._target_id_from_location(
                location,
                precise_only=precise_only,
            )
            if external:
                saw_external = True
            if target_id is not None:
                candidate_ids.add(target_id)
        return sorted(candidate_ids), saw_external

    def _target_id_from_location(
        self,
        location: object,
        *,
        precise_only: bool = False,
    ) -> tuple[str | None, bool]:
        if not isinstance(location, dict):
            return None, False
        raw_uri = location.get("uri")
        raw_range = location.get("range")
        if raw_uri is None:
            raw_uri = location.get("targetUri")
        if raw_range is None:
            raw_range = location.get("targetSelectionRange") or location.get("targetRange")
        if not isinstance(raw_uri, str) or not isinstance(raw_range, dict):
            return None, False
        target_path = _path_from_uri(raw_uri)
        if target_path is None:
            return None, False
        if not self._is_internal_project_path(target_path):
            return None, True
        target_index = self._function_index_for_path(target_path)
        if target_index.parse_status == "syntax_error":
            return None, False
        key = _range_start_key(raw_range)
        if key is not None and key in target_index.entity_by_name_position:
            return target_index.entity_by_name_position[key], False
        if precise_only:
            return None, False
        return target_index.module_id, False

    def _ensure_process(self) -> bool:
        if self._run_state.disabled:
            return False
        if self._process is None:
            # A fresh session (server.py recycles one every
            # ``MAX_FILES_PER_PYRIGHT_SESSION`` files) may be the "next file"
            # a ceiling-deferred restart was handed to. This spawn IS that
            # deferred restart, so consume the one-shot flag here too: left
            # armed, it would later swallow a genuine arrival-death on an
            # unrelated file -- no finding, and a restart the run-level cap
            # never sees (ADR-057).
            self._consume_deferred_restart()
            return self._start_process()
        if self._process.poll() is None:
            return True
        self._terminate_process()
        if self._run_state.restart_already_charged_to_file:
            # The previous file's own request killed pyright and its watchdog
            # ceiling had no headroom for the respawn: it was charged there,
            # so this file pays the spawn silently -- no finding, no
            # collateral mark (ADR-057). (The crashing file's OWN later facet
            # never reaches here: ``_own_deferred_restart_blocks`` stops it.)
            self._consume_deferred_restart()
            return self._start_process()
        self._record_restart_or_poison("pyright subprocess exited")
        if self._run_state.disabled:
            return False
        return self._start_process()

    def _consume_deferred_restart(self) -> None:
        self._run_state.restart_already_charged_to_file = False
        self._run_state.restart_charged_to_path = None

    def _defer_restart_to_next_file(self, path: Path) -> None:
        """Leave pyright down; the next file's ``_ensure_process`` respawns, un-charged."""
        state = self._run_state
        state.restart_already_charged_to_file = True
        state.restart_charged_to_path = str(path)
        state.ceiling_deferred_restart_count += 1

    def _process_confirmed_dead(self) -> bool:
        return self._process is None or self._process.poll() is not None

    def _in_flight_failure_reason(self, exc: BaseException) -> _InFlightFailureReason:
        """Classify an exception caught mid-pass, while THIS file's request ran.

        ``_target_id_from_call`` / ``_target_id_from_location`` ``read_text``
        unrelated target files inside the same try body a pyright death is
        caught in, so a bare ``OSError`` is only a transport failure when the
        process is confirmed dead (or the transport itself said so).
        """
        if isinstance(exc, (LspTransportClosedError, BrokenPipeError)):
            return "pyright_transport_failure"
        if self._process_confirmed_dead():
            return "pyright_transport_failure"
        return "pyright_local_read_error"

    def _record_arrival_death(self, reason: str) -> None:
        """Pyright was dead before this file's document even opened.

        Structurally arrival-only: the ``didOpen`` notify is the first thing
        either pass sends. Drop the dead handle so the next ``_ensure_process``
        spawns fresh instead of counting the same death a second time.
        """
        self._record_restart_or_poison(reason)
        self._terminate_process()

    def _record_restart_or_poison(self, reason: str) -> None:
        """Account for a death NOT attributable to the current file.

        Spends the run-level ``MAX_PYRIGHT_RESTARTS_PER_RUN`` budget. The
        finding is anchored (by the host) to the arriving file, and its message
        says so, because that file is where the death was discovered -- not
        where it happened.
        """
        if not self._charge_run_level_restart(reason, attribution="arrival"):
            return
        self._record_finding(
            FINDING_PYRIGHT_RESTART,
            "pyright subprocess found dead on arrival (it died after the previous "
            "file's request completed); restarting",
            restart_count=self._run_state.restart_count,
            file_attributed_restart_count=self._run_state.file_attributed_restart_count,
            attribution="arrival",
            reason=reason,
        )

    def _charge_run_level_restart(self, reason: str, *, attribution: str) -> bool:
        """Spend one unit of ``MAX_PYRIGHT_RESTARTS_PER_RUN``.

        Returns False -- with the run disabled as ``pyright_poisoned`` and the
        POISON-FRAME finding recorded -- when the cap is exceeded.
        """
        state = self._run_state
        state.restart_count += 1
        if state.restart_count <= self.max_restarts_per_run:
            return True
        state.disabled = True
        state.disabled_reason = "pyright_poisoned"
        self._record_finding(
            FINDING_PYRIGHT_POISON_FRAME,
            "pyright restart cap exceeded; skipping call resolution",
            restart_count=state.restart_count,
            file_attributed_restart_count=state.file_attributed_restart_count,
            attribution=attribution,
            reason=reason,
        )
        return False

    def _total_restart_budget_exceeded(self) -> bool:
        state = self._run_state
        total = state.restart_count + state.file_attributed_restart_count
        return (
            total > self.max_total_restarts_per_run
            or state.pyright_init_latency_total_ms > self.restart_latency_budget_ms
        )

    def _trip_total_restart_cap(self, reason: str) -> None:
        state = self._run_state
        state.disabled = True
        state.disabled_reason = "pyright_restart_cap_exceeded"
        self._terminate_process()
        self._record_finding(
            FINDING_PYRIGHT_TOTAL_RESTART_CAP_EXCEEDED,
            "pyright total restart safety budget exceeded; skipping call resolution",
            restart_count=state.restart_count,
            file_attributed_restart_count=state.file_attributed_restart_count,
            pyright_init_latency_total_ms=state.pyright_init_latency_total_ms,
            max_total_restarts_per_run=self.max_total_restarts_per_run,
            restart_latency_budget_ms=self.restart_latency_budget_ms,
            reason=reason,
        )

    def _note_calls_facet_outcome(self, path: Path, coverage: FacetCoverage) -> None:
        """Feed the wedged-but-alive breaker with this file's calls outcome.

        Counts a FILE once, by the reason its calls pass landed on; the
        references pass never participates. Any other degraded outcome
        (transport failure, local read error) leaves the streak alone: those
        paths either restarted pyright themselves -- which resets the streak
        via ``_start_process`` -- or say nothing about its responsiveness.
        """
        state = self._run_state
        if coverage.reason == "pyright_timeout":
            state.consecutive_timeout_files += 1
            if state.consecutive_timeout_files >= self.max_consecutive_timeout_files:
                self._record_wedged_restart(path)
        elif coverage.status == "complete":
            state.consecutive_timeout_files = 0

    def _record_wedged_restart(self, path: Path) -> None:
        """``path`` closed a timeout streak: presume pyright wedged and restart NOW.

        The process still answers ``poll()``, so nothing else would ever
        restart it and every remaining file would time out. Charged to the
        run-level cap (a wedge loop is a crash loop by another name), bounded
        by the total-restart safety budget, and performed through the same
        headroom-aware respawn path an in-flight death uses, so it defers to
        the next file rather than overrun the host's watchdog. The streak's
        files keep their self-inflicted ``pyright_timeout`` claim.
        """
        state = self._run_state
        streak = state.consecutive_timeout_files
        state.consecutive_timeout_files = 0
        state.wedged_restart_count += 1
        reason = f"pyright timed out on {streak} consecutive files while alive"
        if not self._charge_run_level_restart(reason, attribution="wedged"):
            self._terminate_process()
            outcome: _RestartOutcome | Literal["cap_exceeded"] = "cap_exceeded"
        elif self._total_restart_budget_exceeded():
            self._trip_total_restart_cap(reason)
            outcome = "cap_exceeded"
        else:
            outcome = self._restart_process_for_file(path)
        self._record_finding(
            FINDING_PYRIGHT_WEDGED_RESTART,
            f"pyright timed out on {streak} consecutive files while still alive; "
            f"treating it as wedged and restarting; {outcome}",
            consecutive_timeout_files=streak,
            max_consecutive_timeout_files=self.max_consecutive_timeout_files,
            restart_count=state.restart_count,
            wedged_restart_count=state.wedged_restart_count,
            attribution="wedged",
            outcome=outcome,
            reason=reason,
        )

    def _record_file_attributed_restart(self, path: Path, reason: str) -> None:
        """Pyright died while ``path``'s own request was in flight: restart NOW.

        The crashing file's coverage was built by the caller before this runs,
        so nothing here changes its (self-inflicted) attribution; this only
        decides whether the NEXT file finds a live process and who pays.
        """
        state = self._run_state
        state.file_attributed_restart_count += 1
        if self._total_restart_budget_exceeded():
            self._record_in_flight_restart_finding(reason, outcome="cap_exceeded")
            self._trip_total_restart_cap(reason)
            return
        outcome = self._restart_process_for_file(path)
        if outcome == "respawn_failed":
            # The respawn itself failed. Never force ``disabled`` here: the
            # spawn path already triaged it -- a transient deferral (next file
            # retries) or a permanent disable with its OWN ``disabled_reason``.
            state.file_attributed_respawn_failure_count += 1
        self._record_in_flight_restart_finding(reason, outcome=outcome)

    def _record_in_flight_restart_finding(self, reason: str, *, outcome: str) -> None:
        self._record_finding(
            FINDING_PYRIGHT_RESTART,
            f"pyright subprocess died during this file's request; {outcome}",
            restart_count=self._run_state.restart_count,
            file_attributed_restart_count=self._run_state.file_attributed_restart_count,
            attribution="in_flight",
            outcome=outcome,
            reason=reason,
        )

    def _restart_process_for_file(self, path: Path) -> _RestartOutcome:
        """Respawn after ``path``'s own request killed pyright, within its headroom.

        The decision is made on the REAL headroom left under the file's
        host-watchdog ceiling at this moment. Too little (below
        ``MIN_RESPAWN_HEADROOM_SECS``): defer to the next file -- respawning
        would buy no time and risk the watchdog killing the whole call.
        Otherwise the initialize handshake is bounded to the headroom, so a
        hung respawn is cut off inside the window and deferred rather than
        overrunning the host's real deadline.

        Extends the file's shared calls/references deadline by the respawn's
        latency -- clamped to the ceiling -- so the other facet of the same
        file neither self-inflicts a timeout out of restart latency nor pushes
        the host's per-file watchdog into killing the plugin call.
        """
        self._terminate_process()
        headroom = self._respawn_headroom_secs(path)
        if headroom < MIN_RESPAWN_HEADROOM_SECS:
            self._defer_restart_to_next_file(path)
            return "deferred_to_next_file"
        started = self._now()
        self._bounded_init_timed_out = False
        ok = self._start_process(init_timeout_secs=min(self.init_timeout_secs, headroom))
        elapsed = self._now() - started
        anchor = self._file_started_at.get(path)
        deadline = self._file_deadlines.get(path)
        if anchor is not None and deadline is not None:
            self._file_deadlines[path] = min(deadline + elapsed, self._watchdog_ceiling_for(anchor))
        if ok:
            return "restarted"
        if self._bounded_init_timed_out:
            self._defer_restart_to_next_file(path)
            return "deferred_to_next_file"
        return "respawn_failed"

    def _terminate_process(self) -> None:
        """Drop the current process handle, killing it if it is still alive.

        Also retires its stderr drain so a RESTART finding's ``reason`` never
        mixes two processes' stderr.
        """
        process = self._process
        self._process = None
        if process is not None and process.poll() is None:
            process.kill()
            with contextlib.suppress(subprocess.TimeoutExpired):
                process.wait(timeout=2)
        if self._stderr_thread is not None:
            self._stderr_thread.join(timeout=2)
            self._stderr_thread = None
        self._stderr_tail.clear()

    def _start_process(self, init_timeout_secs: float | None = None) -> bool:
        """Spawn + initialize; ``init_timeout_secs`` bounds the handshake below the default."""
        started = self._now()
        try:
            ok = self._spawn_and_initialize(init_timeout_secs)
        finally:
            elapsed_ms = math.ceil((self._now() - started) * 1000)
            self._run_state.pyright_init_latency_total_ms += elapsed_ms
        if ok:
            # A fresh process owes nothing to the old one's timeouts.
            self._run_state.consecutive_timeout_files = 0
        return ok

    def _spawn_and_initialize(self, init_timeout_secs: float | None = None) -> bool:
        if init_timeout_secs is None:
            init_timeout_secs = self.init_timeout_secs
        executable = self._resolve_executable()
        if executable is None:
            self._run_state.disabled = True
            self._run_state.disabled_reason = "pyright_unavailable"
            self._record_finding(
                FINDING_PYRIGHT_UNAVAILABLE,
                "pyright-langserver is not available",
                executable=self.executable,
            )
            return False
        if self.install_check is not None and not self.install_check(executable):
            self._run_state.disabled = True
            self._run_state.disabled_reason = "pyright_unavailable"
            self._record_finding(
                FINDING_PYRIGHT_INSTALL_FAILURE,
                "pyright-langserver executability check failed",
                executable=executable,
            )
            return False

        preexec_fn = None
        if sys.platform == "linux":
            libc_name = ctypes.util.find_library("c")
            libc = None
            if libc_name is not None:
                try:  # noqa: SIM105
                    libc = ctypes.CDLL(libc_name, use_errno=True)
                except Exception:  # noqa: BLE001, S110
                    pass

            if libc is not None:

                def set_pdeathsig() -> None:
                    try:
                        # PR_SET_PDEATHSIG is 1
                        libc.prctl(1, signal.SIGTERM, 0, 0, 0)
                        if os.getppid() == 1:
                            os._exit(0)
                    except Exception:  # noqa: BLE001, S110
                        pass

                preexec_fn = set_pdeathsig

        try:
            process = subprocess.Popen(  # noqa: S603 - executable path comes from manifest/PATH.
                [executable, "--stdio"],
                cwd=self.project_root,
                env=self._subprocess_env(),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                preexec_fn=preexec_fn,  # noqa: PLW1509
            )
        except OSError as exc:
            return self._handle_spawn_oserror(exc, executable)

        self._process = process
        self._start_stderr_drain(process)
        try:
            self._initialize(init_timeout_secs)
        except LspTimeoutError:
            return self._handle_initialize_timeout(init_timeout_secs)
        except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
            self._run_state.disabled = True
            self._run_state.disabled_reason = "pyright_unavailable"
            self._record_finding(
                FINDING_PYRIGHT_UNAVAILABLE,
                "pyright initialize handshake failed",
                error=str(exc),
            )
            self._terminate_process()
            return False
        # A clean spawn + handshake clears any accumulated transient-deferral
        # pressure: the per-UID resource squeeze that caused earlier EAGAINs has
        # eased, so the run is healthy again.
        self._run_state.consecutive_spawn_deferrals = 0
        return True

    def _handle_spawn_oserror(self, exc: OSError, executable: str) -> bool:
        """Triage a ``subprocess.Popen`` failure into transient vs. permanent.

        ``EAGAIN``/``ENOMEM``/``EMFILE``/``ENFILE`` are *transient*
        resource-pressure errors: a busy host momentarily out of process slots,
        memory, or file descriptors. The spawn is deferred — ``self._process``
        stays ``None`` and ``disabled`` is left unset, so the next file retries a
        fresh spawn — and only a sustained run of deferrals
        (``MAX_CONSECUTIVE_SPAWN_DEFERRALS``) gives up. Any other errno (notably
        ``ENOENT``/``EACCES``) is a genuine, permanent install defect and
        disables pyright for the rest of the run.
        """
        if exc.errno in _TRANSIENT_SPAWN_ERRNOS:
            self._run_state.consecutive_spawn_deferrals += 1
            if self._run_state.consecutive_spawn_deferrals > MAX_CONSECUTIVE_SPAWN_DEFERRALS:
                self._run_state.disabled = True
                self._run_state.disabled_reason = "pyright_unavailable"
                self._record_finding(
                    FINDING_PYRIGHT_RESOURCE_EXHAUSTED,
                    "pyright-langserver persistently unavailable under resource "
                    "pressure; skipping call resolution",
                    executable=executable,
                    consecutive_spawn_deferrals=self._run_state.consecutive_spawn_deferrals,
                    error=str(exc),
                )
                return False
            # Emit one finding per pressure *episode* (the 0 -> 1 transition),
            # not one per deferred file, so a busy run is not buried in findings.
            if self._run_state.consecutive_spawn_deferrals == 1:
                self._record_finding(
                    FINDING_PYRIGHT_SPAWN_DEFERRED,
                    "pyright-langserver spawn deferred under resource pressure; "
                    "will retry on subsequent files",
                    executable=executable,
                    error=str(exc),
                )
            return False
        self._run_state.disabled = True
        self._run_state.disabled_reason = "pyright_unavailable"
        self._record_finding(
            FINDING_PYRIGHT_INSTALL_FAILURE,
            "pyright-langserver failed to start",
            executable=executable,
            error=str(exc),
        )
        return False

    def _handle_initialize_timeout(self, init_timeout_secs: float) -> bool:
        """Triage a handshake timeout: a headroom-bounded one is a deferral, not a disable."""
        self._terminate_process()
        if init_timeout_secs < self.init_timeout_secs:
            # The respawn was cut off by a crashing file's remaining watchdog
            # headroom, not by pyright's own budget: the caller defers it to
            # the next file. Disabling the run here would turn one late crash
            # into a poisoned run.
            self._bounded_init_timed_out = True
            return False
        self._run_state.disabled = True
        self._run_state.disabled_reason = "pyright_unavailable"
        self._record_finding(
            FINDING_PYRIGHT_INIT_TIMEOUT,
            "pyright initialize handshake timed out",
            timeout_secs=init_timeout_secs,
        )
        return False

    def _initialize(self, timeout_secs: float | None = None) -> None:
        result = self._request(
            "initialize",
            {
                "processId": os.getpid(),
                "rootUri": self.project_root.as_uri(),
                "workspaceFolders": [
                    {"uri": self.project_root.as_uri(), "name": self.project_root.name},
                ],
                "capabilities": {"workspace": {"configuration": True}},
                "clientInfo": {"name": "loomweave-plugin-python", "version": __version__},
            },
            self.init_timeout_secs if timeout_secs is None else timeout_secs,
        )
        _ = result
        self._notify("initialized", {})

    def _resolve_executable(self) -> str | None:
        candidate = Path(self.executable)
        if candidate.parent != Path() or candidate.is_absolute():
            return str(candidate) if candidate.exists() else None
        sibling = Path(sys.executable).parent / self.executable
        if sibling.exists():
            return str(sibling)
        return shutil.which(self.executable)

    def _subprocess_env(self) -> dict[str, str]:
        if self.env is None:
            return os.environ.copy()
        merged = os.environ.copy()
        merged.update(self.env)
        return merged

    def _start_stderr_drain(self, process: subprocess.Popen[bytes]) -> None:
        stderr = process.stderr
        if stderr is None:
            return
        thread = threading.Thread(target=self._drain_stderr, args=(stderr,), daemon=True)
        thread.start()
        self._stderr_thread = thread

    def _drain_stderr(self, stderr: IO[bytes]) -> None:
        while True:
            chunk = stderr.read(8192)
            if not chunk:
                return
            self._stderr_tail.extend(chunk)
            if len(self._stderr_tail) > STDERR_TAIL_LIMIT:
                del self._stderr_tail[:-STDERR_TAIL_LIMIT]

    def _request(self, method: str, params: dict[str, object], timeout_secs: float) -> object:
        process = self._live_process()
        request_id = self._next_id
        self._next_id += 1
        self._write_message(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            },
        )
        # ``timeout_secs`` bounds the WHOLE request, not each read
        # (clarion-7fc41105ea). Server-initiated traffic between the request
        # and its response -- logMessage, publishDiagnostics,
        # workspace/configuration -- must never reset the clock: with a fresh
        # grant per message, a chatty pyright stretches a single "budgeted"
        # query arbitrarily far past the file deadline, and with it past the
        # host-watchdog ceiling every headroom computation in this file is
        # built on. That is how elspeth's service.py call outlived its own
        # 105s window and was killed by the host's 120s watchdog.
        deadline = self._now() + timeout_secs
        while True:
            response = self._read_message(deadline - self._now())
            if "method" in response:
                self._handle_server_message(response)
                continue
            if response.get("id") != request_id:
                continue
            if "error" in response:
                raise LspTransportClosedError(str(response["error"]))
            process.poll()
            return response.get("result")

    def _handle_server_message(self, message: dict[str, Any]) -> None:
        if "id" not in message:
            return
        request_id = message["id"]
        method = message.get("method")
        if method == "workspace/configuration":
            result = self._workspace_configuration_result(message)
        else:
            result = None
        self._write_message({"jsonrpc": "2.0", "id": request_id, "result": result})

    def _workspace_configuration_result(self, message: dict[str, Any]) -> list[object]:
        params = message.get("params")
        items = params.get("items") if isinstance(params, dict) else None
        if not isinstance(items, list):
            return []
        return [self._configuration_for_section(item) for item in items]

    def _configuration_for_section(self, item: object) -> object:
        section = item.get("section") if isinstance(item, dict) else None
        analysis = {
            "diagnosticMode": "openFilesOnly",
            "exclude": PYRIGHT_EXCLUDE_PATTERNS,
            "indexing": False,
            "useLibraryCodeForTypes": False,
        }
        if section == "python":
            return {"analysis": analysis}
        if section == "python.analysis":
            return analysis
        if section == "pyright":
            return {}
        return None

    def _notify(self, method: str, params: dict[str, object]) -> None:
        self._live_process()
        self._write_message({"jsonrpc": "2.0", "method": method, "params": params})

    def _live_process(self) -> subprocess.Popen[bytes]:
        if self._process is None or self._process.poll() is not None:
            message = "pyright subprocess is not running"
            raise LspTransportClosedError(message)
        return self._process

    def _write_message(self, message: dict[str, object]) -> None:
        process = self._live_process()
        if process.stdin is None:
            error_message = "pyright stdin is closed"
            raise LspTransportClosedError(error_message)
        body = json.dumps(message, separators=(",", ":")).encode("utf-8")
        header = f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
        process.stdin.write(header)
        process.stdin.write(body)
        process.stdin.flush()

    def _read_message(self, timeout_secs: float) -> dict[str, Any]:
        process = self._live_process()
        if process.stdout is None:
            message = "pyright stdout is closed"
            raise LspTransportClosedError(message)
        fd = process.stdout.fileno()
        deadline = time.monotonic() + timeout_secs
        headers: dict[str, str] = {}
        while True:
            line = _read_line(fd, deadline)
            if line in (b"\r\n", b"\n"):
                break
            decoded_line = line.decode("ascii", errors="ignore").strip()
            name, sep, value = decoded_line.partition(":")
            if not sep:
                continue
            headers[name.strip().lower()] = value.strip()
        if "content-length" not in headers:
            message = f"missing LSP Content-Length header: {headers!r}"
            raise LspTransportClosedError(message)
        length = int(headers["content-length"])
        body = _read_exact(fd, length, deadline)
        parsed: dict[str, Any] = json.loads(body)
        return parsed

    def _target_id_from_call(self, call: dict[object, object]) -> str | None:
        raw_to = call.get("to")
        if not isinstance(raw_to, dict):
            return None
        raw_uri = raw_to.get("uri")
        raw_selection = raw_to.get("selectionRange")
        if not isinstance(raw_uri, str) or not isinstance(raw_selection, dict):
            return None
        target_path = _path_from_uri(raw_uri)
        if target_path is None:
            return None
        if not self._is_internal_project_path(target_path):
            return None
        index = self._function_index_for_path(target_path)
        if index.parse_status == "syntax_error":
            return None
        key = _range_start_key(raw_selection)
        if key is not None and key in index.by_name_position:
            return index.by_name_position[key].entity_id
        return _containing_function_id(index, raw_selection)

    def _is_internal_project_path(self, path: Path) -> bool:
        if not path.is_relative_to(self.project_root):
            return False
        relative = path.relative_to(self.project_root)
        return not any(part in PROJECT_LOCAL_EXTERNAL_DIRS for part in relative.parts)

    def _function_index_for_path(self, path: Path) -> _FunctionIndex:
        resolved = path.resolve()
        cached = self._function_indexes.get(resolved)
        if cached is not None:
            return cached
        source = resolved.read_text(encoding="utf-8")
        index = _build_function_index(self.project_root, resolved, source)
        self._function_indexes[resolved] = index
        self._index_parse_latency_ms.append(index.parse_latency_ms)
        return index

    def _record_finding(self, subcode: str, message: str, **metadata: object) -> None:
        self._findings.append(
            {
                "subcode": subcode,
                "severity": "warning",
                "message": message,
                "metadata": metadata,
            },
        )

    def _pop_findings(self) -> list[Finding]:
        findings = self._findings
        self._findings = []
        return findings

    def _pop_index_parse_latencies(self) -> list[int]:
        latencies = self._index_parse_latency_ms
        self._index_parse_latency_ms = []
        return latencies


def _build_function_index(project_root: Path, path: Path, source: str) -> _FunctionIndex:
    relative = path.relative_to(project_root) if path.is_relative_to(project_root) else path
    dotted_module = module_dotted_name(relative.as_posix())
    parse_started = time.perf_counter()
    parse_status: Literal["ok", "syntax_error"] = "ok"
    try:
        tree = ast.parse(source)
    except SyntaxError:
        tree = ast.Module(body=[], type_ignores=[])
        parse_status = "syntax_error"
    parse_latency_ms = max(1, math.ceil((time.perf_counter() - parse_started) * 1000))
    functions: list[_FunctionInfo] = []
    entities: list[_EntityInfo] = []
    source_lines = source.splitlines()
    _collect_entities(tree, [tree], dotted_module, source_lines, functions, entities, set())
    line_starts = _line_starts(source)
    module_id = entity_id("python", "module", dotted_module)
    by_id = {function.entity_id: function for function in functions}
    by_name_position = {(function.line, function.character): function for function in functions}
    entity_by_name_position = {
        (entity.line, entity.character): entity.entity_id for entity in entities
    }
    by_short_name = {function.name: function.entity_id for function in functions}
    dunder_call_by_class = _dunder_call_targets(functions)
    return _FunctionIndex(
        source=source,
        line_starts=line_starts,
        parse_latency_ms=parse_latency_ms,
        module_id=module_id,
        by_id=by_id,
        by_name_position=by_name_position,
        entity_by_name_position=entity_by_name_position,
        by_short_name=by_short_name,
        dunder_call_by_class=dunder_call_by_class,
        functions=tuple(functions),
        entities=tuple(entities),
        tree=tree,
        parse_status=parse_status,
    )


def _declaration_name_character(
    line_text: str,
    expected_name: str,
    declaration_kind: Literal["function", "class"],
) -> int:
    keyword = "def" if declaration_kind == "function" else "class"
    try:
        tokens = tokenize.generate_tokens(StringIO(line_text).readline)
        seen_keyword = False
        for token in tokens:
            if token.type != tokenize.NAME:
                continue
            if not seen_keyword:
                if token.string == keyword:
                    seen_keyword = True
                continue
            if token.string == expected_name:
                return token.start[1]
    except tokenize.TokenError:
        return -1
    return -1


def _collect_entities(  # noqa: PLR0913 - keeps function/class indexes in one traversal.
    node: ast.AST,
    parents: list[ast.AST],
    dotted_module: str,
    source_lines: list[str],
    out: list[_FunctionInfo],
    out_entities: list[_EntityInfo],
    seen_ids: set[str],
) -> None:
    for child in ast.iter_child_nodes(node):
        match child:
            case ast.FunctionDef() | ast.AsyncFunctionDef():
                if _has_overload_decorator(child):
                    continue
                python_qualname = reconstruct_qualname(child, parents)
                qualified_name = f"{dotted_module}.{python_qualname}"
                child_id = entity_id("python", "function", qualified_name)
                if child_id in seen_ids:
                    continue
                seen_ids.add(child_id)
                line_text = (
                    source_lines[child.lineno - 1] if child.lineno <= len(source_lines) else ""
                )
                name_character = _declaration_name_character(line_text, child.name, "function")
                character = (
                    _codepoint_col_to_utf16(line_text, name_character)
                    if name_character >= 0
                    else _byte_col_to_utf16(line_text, child.col_offset)
                )
                entity = _EntityInfo(
                    entity_id=child_id,
                    line=child.lineno - 1,
                    character=character,
                )
                out_entities.append(entity)
                out.append(
                    _FunctionInfo(
                        entity_id=entity.entity_id,
                        qualified_name=qualified_name,
                        name=child.name,
                        line=child.lineno - 1,
                        character=character,
                        end_line=(child.end_lineno or child.lineno) - 1,
                        end_character=_ast_position_to_lsp(
                            source_lines,
                            (child.end_lineno or child.lineno) - 1,
                            child.end_col_offset or child.col_offset,
                        ),
                        call_sites=tuple(_function_call_sites(child, source_lines)),
                        node=child,
                    ),
                )
                _collect_entities(
                    child,
                    [*parents, child],
                    dotted_module,
                    source_lines,
                    out,
                    out_entities,
                    seen_ids,
                )
            case ast.ClassDef():
                python_qualname = reconstruct_qualname(child, parents)
                qualified_name = f"{dotted_module}.{python_qualname}"
                child_id = entity_id("python", "class", qualified_name)
                if child_id in seen_ids:
                    continue
                seen_ids.add(child_id)
                line_text = (
                    source_lines[child.lineno - 1] if child.lineno <= len(source_lines) else ""
                )
                name_character = _declaration_name_character(line_text, child.name, "class")
                character = (
                    _codepoint_col_to_utf16(line_text, name_character)
                    if name_character >= 0
                    else _byte_col_to_utf16(line_text, child.col_offset)
                )
                out_entities.append(
                    _EntityInfo(
                        entity_id=child_id,
                        line=child.lineno - 1,
                        character=character,
                    ),
                )
                _collect_entities(
                    child,
                    [*parents, child],
                    dotted_module,
                    source_lines,
                    out,
                    out_entities,
                    seen_ids,
                )
            case _:
                _collect_entities(
                    child,
                    [*parents, child],
                    dotted_module,
                    source_lines,
                    out,
                    out_entities,
                    seen_ids,
                )


def _has_overload_decorator(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    for decorator in node.decorator_list:
        match decorator:
            case ast.Name(id="overload"):
                return True
            case ast.Attribute(
                value=ast.Name(id="typing" | "typing_extensions"),
                attr="overload",
            ):
                return True
    return False


def _merge_reference_site(
    accumulators: dict[tuple[str, str, str], _ReferenceEdgeAccumulator],
    site: ReferenceSite,
    candidate_ids: Sequence[str],
) -> None:
    """Fold one resolved site into the per-file edge accumulators.

    The site kind selects the edge kind (``_EDGE_KIND_BY_SITE_KIND``).
    ``decorator`` sites invert direction: the site owner is the *decorated*
    entity, but the stored edge reads ``decorator decorates decorated``
    (ADR-051: from_id = decorator entity, to_id = decorated entity), so the
    resolved candidate becomes ``from_id``. Ambiguous candidates therefore
    list alternative decorators (from-side) rather than alternative targets.
    """
    sorted_candidates = sorted(set(candidate_ids))
    edge_kind = _EDGE_KIND_BY_SITE_KIND[site.kind]
    if site.kind == "decorator":
        from_id, to_id = sorted_candidates[0], site.from_id
    else:
        from_id, to_id = site.from_id, sorted_candidates[0]
    key = (edge_kind, from_id, to_id)
    existing = accumulators.get(key)
    if existing is None:
        accumulators[key] = _ReferenceEdgeAccumulator(
            kind=edge_kind,
            from_id=from_id,
            to_id=to_id,
            source_byte_start=site.source_byte_start,
            source_byte_end=site.source_byte_end,
            candidates=set(sorted_candidates),
        )
        return
    existing.candidates.update(sorted_candidates)
    if (site.source_byte_start, site.source_byte_end) < (
        existing.source_byte_start,
        existing.source_byte_end,
    ):
        existing.source_byte_start = site.source_byte_start
        existing.source_byte_end = site.source_byte_end


def _filter_relation_candidates(site: ReferenceSite, candidate_ids: list[str]) -> list[str]:
    """Apply the relation-site target discipline (Rust derives/implements parity).

    ``inherits_from`` targets must be class entities — a base name resolving
    to a function (factory alias, shadowing ``def``) is dropped rather than
    stored as a class-inherits-function fact, mirroring the Rust resolver's
    ``rust:trait:`` kind filter. Both relation kinds drop self-edges
    (``class X(X)`` resolving the in-definition name to itself).
    """
    if site.kind == "base":
        candidate_ids = [cid for cid in candidate_ids if cid.startswith("python:class:")]
    if site.kind in ("base", "decorator"):
        candidate_ids = [cid for cid in candidate_ids if cid != site.from_id]
    return candidate_ids


def _reference_lookup_cache_key(
    site: ReferenceSite,
    source_bytes: bytes,
) -> tuple[str, str, str, int, int, int, int]:
    lexeme = source_bytes[site.source_byte_start : site.source_byte_end].decode("utf-8")
    return (
        site.from_id,
        site.kind,
        lexeme,
        site.line,
        site.character,
        site.source_byte_start,
        site.source_byte_end,
    )


def _reference_root_name(site: ReferenceSite, source_bytes: bytes) -> str:
    lexeme = source_bytes[site.source_byte_start : site.source_byte_end].decode("utf-8")
    return lexeme.split(".", 1)[0]


def _file_bound_names(tree: ast.Module) -> set[str]:
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            names.add(node.name)
        elif isinstance(node, ast.arg):
            names.add(node.arg)
        elif isinstance(node, ast.Name) and isinstance(node.ctx, (ast.Store, ast.Del)):
            names.add(node.id)
        elif isinstance(node, ast.Import):
            names.update(alias.asname or alias.name.split(".", 1)[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            names.update(alias.asname or alias.name for alias in node.names if alias.name != "*")
        elif (
            isinstance(node, (ast.ExceptHandler, ast.MatchAs, ast.MatchStar))
            and node.name is not None
        ):
            names.add(node.name)
        elif isinstance(node, ast.MatchMapping) and node.rest is not None:
            names.add(node.rest)
        elif type(node).__name__ in _TYPE_PARAMETER_NODE_NAMES:
            type_parameter_name = getattr(node, "name", None)
            if isinstance(type_parameter_name, str):
                names.add(type_parameter_name)
    return names


def _reference_fast_path_names(tree: ast.Module) -> set[str]:
    has_star_import = any(
        isinstance(node, ast.ImportFrom) and any(alias.name == "*" for alias in node.names)
        for node in ast.walk(tree)
    )
    if has_star_import:
        return set()
    # One nested shadow disables a builtin shortcut for the whole file. The
    # lost optimization is cheaper than maintaining a lexical oracle beside Pyright.
    return set(_BUILTIN_NAMES - _file_bound_names(tree))


def _sorted_reference_accumulators(
    accumulators: dict[tuple[str, str, str], _ReferenceEdgeAccumulator],
) -> list[_ReferenceEdgeAccumulator]:
    return sorted(
        accumulators.values(),
        key=lambda acc: (
            acc.source_byte_start,
            acc.source_byte_end,
            acc.from_id,
            acc.to_id,
        ),
    )


def _reference_accumulator_to_edge(
    accumulator: _ReferenceEdgeAccumulator,
) -> ReferencesRawEdge:
    candidates = sorted(accumulator.candidates)
    edge: ReferencesRawEdge = {
        "kind": accumulator.kind,
        "from_id": accumulator.from_id,
        "to_id": accumulator.to_id,
        "source_byte_start": accumulator.source_byte_start,
        "source_byte_end": accumulator.source_byte_end,
        "confidence": "resolved" if len(candidates) == 1 else "ambiguous",
    }
    if len(candidates) > 1:
        edge["properties"] = {"candidates": candidates}
    return edge


def _function_call_sites(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    source_lines: Sequence[str],
) -> list[_CallSite]:
    visitor = _CallSiteVisitor(source_lines)
    for statement in node.body:
        visitor.visit(statement)
    return visitor.call_sites


def _unresolved_call_site_total_for_function(
    function: _FunctionInfo,
    resolved_ranges: set[tuple[int, int, int, int]],
) -> int:
    return sum(
        1
        for call_site in function.call_sites
        if (
            call_site.line,
            call_site.character,
            call_site.end_line,
            call_site.end_character,
        )
        not in resolved_ranges
    )


def _unresolved_call_sites_for_function(
    index: _FunctionIndex,
    function: _FunctionInfo,
    resolved_ranges: set[tuple[int, int, int, int]],
) -> list[UnresolvedCallSite]:
    unresolved: list[UnresolvedCallSite] = []
    for site_ordinal, call_site in enumerate(function.call_sites):
        range_key = (
            call_site.line,
            call_site.character,
            call_site.end_line,
            call_site.end_character,
        )
        if range_key in resolved_ranges:
            continue
        if len(call_site.callee_expr.encode("utf-8")) > MAX_UNRESOLVED_CALLEE_EXPR_BYTES:
            continue
        start_byte = _position_to_byte(index, call_site.line, call_site.character)
        end_byte = _position_to_byte(index, call_site.end_line, call_site.end_character)
        unresolved.append(
            {
                "caller_entity_id": function.entity_id,
                "site_ordinal": site_ordinal,
                "source_byte_start": start_byte,
                "source_byte_end": end_byte,
                "callee_expr": call_site.callee_expr,
            },
        )
    return unresolved


class _CallSiteVisitor(ast.NodeVisitor):
    def __init__(self, source_lines: Sequence[str]) -> None:
        self.source_lines = source_lines
        self.call_sites: list[_CallSite] = []

    def visit_Call(self, node: ast.Call) -> None:
        func = node.func
        callee_expr = ast.unparse(func)
        self.call_sites.append(
            _CallSite(
                func.lineno - 1,
                _ast_position_to_lsp(
                    self.source_lines,
                    func.lineno - 1,
                    func.col_offset,
                ),
                (func.end_lineno or func.lineno) - 1,
                _ast_position_to_lsp(
                    self.source_lines,
                    (func.end_lineno or func.lineno) - 1,
                    func.end_col_offset or func.col_offset,
                ),
                callee_expr,
            ),
        )
        self.generic_visit(node)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        _ = node

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        _ = node

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        _ = node


def _ambiguous_dict_dispatches(
    index: _FunctionIndex,
    function: _FunctionInfo,
) -> dict[tuple[int, int, int, int], set[str]]:
    candidate_maps = _callable_dict_maps(index, function.node)
    if not candidate_maps:
        return {}
    visitor = _DictDispatchVisitor(candidate_maps, index.source.splitlines())
    for statement in function.node.body:
        visitor.visit(statement)
    return visitor.dispatches


def _dunder_call_dispatches(
    index: _FunctionIndex,
    function: _FunctionInfo,
) -> dict[tuple[int, int, int, int], set[str]]:
    if not index.dunder_call_by_class:
        return {}
    visitor = _DunderCallDispatchVisitor(
        index.dunder_call_by_class,
        index.source.splitlines(),
    )
    for statement in function.node.body:
        visitor.visit(statement)
    return visitor.dispatches


def _dunder_call_targets(functions: list[_FunctionInfo]) -> dict[str, str]:
    targets: dict[str, str] = {}
    for function in functions:
        if not function.qualified_name.endswith(".__call__"):
            continue
        class_name = function.qualified_name.rsplit(".", 2)[-2]
        targets[class_name] = function.entity_id
    return targets


def _callable_dict_maps(
    index: _FunctionIndex,
    function: ast.FunctionDef | ast.AsyncFunctionDef,
) -> dict[str, set[str]]:
    maps: dict[str, set[str]] = {}
    for body in [index.tree.body, function.body]:
        for statement in body:
            name, value = _callable_dict_assignment(statement, index.by_short_name)
            if name is not None and value:
                maps[name] = value
    return maps


def _callable_dict_assignment(
    statement: ast.stmt,
    by_short_name: dict[str, str],
) -> tuple[str | None, set[str]]:
    target: ast.expr | None = None
    value: ast.expr | None = None
    match statement:
        case ast.Assign(targets=[ast.Name() as name], value=ast.Dict() as dict_value):
            target = name
            value = dict_value
        case ast.AnnAssign(target=ast.Name() as name, value=ast.Dict() as dict_value):
            target = name
            value = dict_value
        case _:
            return None, set()
    candidates: set[str] = set()
    if isinstance(value, ast.Dict):
        for item in value.values:
            if isinstance(item, ast.Name) and item.id in by_short_name:
                candidates.add(by_short_name[item.id])
    if isinstance(target, ast.Name):
        return target.id, candidates
    return None, candidates


class _DictDispatchVisitor(ast.NodeVisitor):
    def __init__(
        self,
        candidate_maps: dict[str, set[str]],
        source_lines: Sequence[str],
    ) -> None:
        self.candidate_maps = candidate_maps
        self.source_lines = source_lines
        self.dispatches: dict[tuple[int, int, int, int], set[str]] = {}

    def visit_Call(self, node: ast.Call) -> None:
        func = node.func
        if (
            isinstance(func, ast.Subscript)
            and isinstance(func.value, ast.Name)
            and func.value.id in self.candidate_maps
        ):
            key = (
                func.lineno - 1,
                _ast_position_to_lsp(
                    self.source_lines,
                    func.lineno - 1,
                    func.col_offset,
                ),
                (func.end_lineno or func.lineno) - 1,
                _ast_position_to_lsp(
                    self.source_lines,
                    (func.end_lineno or func.lineno) - 1,
                    func.end_col_offset or func.col_offset,
                ),
            )
            self.dispatches[key] = set(self.candidate_maps[func.value.id])
        self.generic_visit(node)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        _ = node

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        _ = node

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        _ = node


class _DunderCallDispatchVisitor(ast.NodeVisitor):
    def __init__(
        self,
        dunder_call_by_class: dict[str, str],
        source_lines: Sequence[str],
    ) -> None:
        self.dunder_call_by_class = dunder_call_by_class
        self.source_lines = source_lines
        self.instance_targets: dict[str, str] = {}
        self.dispatches: dict[tuple[int, int, int, int], set[str]] = {}

    def visit_Assign(self, node: ast.Assign) -> None:
        if (
            len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
            and isinstance(node.value, ast.Call)
            and isinstance(node.value.func, ast.Name)
            and node.value.func.id in self.dunder_call_by_class
        ):
            self.instance_targets[node.targets[0].id] = self.dunder_call_by_class[
                node.value.func.id
            ]
        self.generic_visit(node)

    def visit_Call(self, node: ast.Call) -> None:
        func = node.func
        if isinstance(func, ast.Name) and func.id in self.instance_targets:
            key = (
                func.lineno - 1,
                _ast_position_to_lsp(
                    self.source_lines,
                    func.lineno - 1,
                    func.col_offset,
                ),
                (func.end_lineno or func.lineno) - 1,
                _ast_position_to_lsp(
                    self.source_lines,
                    (func.end_lineno or func.lineno) - 1,
                    func.end_col_offset or func.col_offset,
                ),
            )
            self.dispatches[key] = {self.instance_targets[func.id]}
        self.generic_visit(node)

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        _ = node

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        _ = node

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        _ = node


def _line_starts(source: str) -> tuple[int, ...]:
    starts = [0]
    total = 0
    for line in source.splitlines(keepends=True):
        total += len(line.encode("utf-8"))
        starts.append(total)
    return tuple(starts)


def _utf16_units(text: str) -> int:
    return len(text.encode("utf-16-le")) // 2


def _byte_col_to_utf16(line_text: str, byte_col: int) -> int:
    line_bytes = line_text.encode("utf-8")
    prefix = line_bytes[: max(0, min(byte_col, len(line_bytes)))]
    return _utf16_units(prefix.decode("utf-8", errors="ignore"))


def _codepoint_col_to_utf16(line_text: str, codepoint_col: int) -> int:
    return _utf16_units(line_text[: max(0, codepoint_col)])


def _ast_position_to_lsp(
    source_lines: Sequence[str],
    line: int,
    byte_col: int,
) -> int:
    if line < 0 or line >= len(source_lines):
        return 0
    return _byte_col_to_utf16(source_lines[line], byte_col)


def _utf16_col_to_byte(line_text: str, utf16_col: int) -> int:
    target = max(0, utf16_col)
    units = 0
    byte_count = 0
    for char in line_text:
        char_units = _utf16_units(char)
        if units + char_units > target:
            break
        units += char_units
        byte_count += len(char.encode("utf-8"))
        if units == target:
            break
    return byte_count


def _position_to_byte(index: _FunctionIndex, line: int, character: int) -> int:
    if line >= len(index.line_starts):
        return len(index.source.encode("utf-8"))
    line_start = index.line_starts[line]
    line_text = index.source.splitlines(keepends=True)[line] if index.source else ""
    return line_start + _utf16_col_to_byte(line_text, character)


def _range_key(raw_range: object) -> tuple[int, int, int, int] | None:
    if not isinstance(raw_range, dict):
        return None
    start = raw_range.get("start")
    end = raw_range.get("end")
    if not isinstance(start, dict) or not isinstance(end, dict):
        return None
    start_line = start.get("line")
    start_character = start.get("character")
    end_line = end.get("line")
    end_character = end.get("character")
    if not isinstance(start_line, int):
        return None
    if not isinstance(start_character, int):
        return None
    if not isinstance(end_line, int):
        return None
    if not isinstance(end_character, int):
        return None
    return (start_line, start_character, end_line, end_character)


def _range_within_function(
    range_key: tuple[int, int, int, int],
    function: _FunctionInfo,
) -> bool:
    start_line, start_character, end_line, end_character = range_key
    if start_line < function.line or end_line > function.end_line:
        return False
    if start_line == function.line and start_character < function.character:
        return False
    return not (end_line == function.end_line and end_character > function.end_character)


def _range_start_key(raw_range: dict[object, object]) -> tuple[int, int] | None:
    start = raw_range.get("start")
    if not isinstance(start, dict):
        return None
    line = start.get("line")
    character = start.get("character")
    if isinstance(line, int) and isinstance(character, int):
        return (line, character)
    return None


def _containing_function_id(index: _FunctionIndex, raw_range: dict[object, object]) -> str | None:
    key = _range_start_key(raw_range)
    if key is None:
        return None
    line, character = key
    candidates: list[_FunctionInfo] = []
    for function in index.functions:
        starts_inside = function.line < line or (
            function.line == line and character >= function.character
        )
        ends_inside = line < function.end_line or (
            line == function.end_line and character <= function.end_character
        )
        if starts_inside and ends_inside:
            candidates.append(function)
    if not candidates:
        return None
    return min(
        candidates,
        key=lambda function: (
            function.end_line - function.line,
            function.end_character - function.character,
        ),
    ).entity_id


def _path_from_uri(uri: str) -> Path | None:
    parsed = urlparse(uri)
    if parsed.scheme != "file":
        return None
    return Path(unquote(parsed.path)).resolve()


def _read_line(fd: int, deadline: float) -> bytes:
    chunks = bytearray()
    while True:
        _wait_readable(fd, deadline)
        chunk = os.read(fd, 1)
        if not chunk:
            message = "EOF while reading LSP header"
            raise LspTransportClosedError(message)
        chunks.extend(chunk)
        if chunk == b"\n":
            return bytes(chunks)


def _read_exact(fd: int, length: int, deadline: float) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        _wait_readable(fd, deadline)
        chunk = os.read(fd, length - len(chunks))
        if not chunk:
            message = "EOF while reading LSP body"
            raise LspTransportClosedError(message)
        chunks.extend(chunk)
    return bytes(chunks)


def _wait_readable(fd: int, deadline: float) -> None:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        message = "LSP read"
        raise LspTimeoutError(message)
    ready, _, _ = select.select([fd], [], [], remaining)
    if not ready:
        message = "LSP read"
        raise LspTimeoutError(message)
