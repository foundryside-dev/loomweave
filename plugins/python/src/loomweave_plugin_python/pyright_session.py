from __future__ import annotations

import ast
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
    """

    restart_count: int = 0
    disabled: bool = False
    consecutive_spawn_deferrals: int = 0


MAX_UNRESOLVED_CALLEE_EXPR_BYTES = 512
MAX_PYRIGHT_RESTARTS_PER_RUN = 3
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
MAX_REFERENCE_SITES_PER_FILE = 2000
PYRIGHT_INIT_TIMEOUT_SECS = 30.0
PYRIGHT_CALL_TIMEOUT_SECS = 5.0
PYRIGHT_FILE_TIMEOUT_SECS = 3.0
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


if TYPE_CHECKING:
    from collections.abc import Callable, Sequence


class LspTimeoutError(TimeoutError):
    def __init__(self, method: str) -> None:
        super().__init__(f"{method} timed out")
        self.method = method


class LspTransportClosedError(RuntimeError):
    pass


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
        file_timeout_secs: float = PYRIGHT_FILE_TIMEOUT_SECS,
        max_restarts_per_run: int = MAX_PYRIGHT_RESTARTS_PER_RUN,
        max_reference_sites_per_file: int = MAX_REFERENCE_SITES_PER_FILE,
        run_state: PyrightRunState | None = None,
    ) -> None:
        self.project_root = Path(project_root).resolve()
        self.executable = executable
        self.env = env
        self.install_check = install_check
        self.init_timeout_secs = init_timeout_secs
        self.call_timeout_secs = call_timeout_secs
        self.file_timeout_secs = file_timeout_secs
        self.max_restarts_per_run = max_restarts_per_run
        self.max_reference_sites_per_file = max_reference_sites_per_file
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
        index = self._function_index_for_path(path)
        if index.parse_status == "syntax_error":
            return CallResolutionResult(
                unresolved_call_sites_total=len(function_ids),
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
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

        if not self._ensure_process():
            return CallResolutionResult(
                unresolved_call_sites_total=ast_call_sites_total,
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
            )

        deadline = self._deadline_for_file(path)
        latency_started = time.perf_counter()
        try:
            edges, unresolved, unresolved_sites = self._resolve_with_pyright(
                path,
                index,
                requested,
                deadline,
            )
        except LspTimeoutError as exc:
            self._record_finding(
                FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT,
                f"pyright query timed out: {exc.method}",
                method=exc.method,
            )
            edges = []
            unresolved = ast_call_sites_total
            unresolved_sites = []
        except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
            self._record_restart_or_poison(str(exc))
            edges = []
            unresolved = ast_call_sites_total
            unresolved_sites = []
        latency_ms = max(1, math.ceil((time.perf_counter() - latency_started) * 1000))

        return CallResolutionResult(
            edges=edges,
            unresolved_call_sites_total=unresolved,
            unresolved_call_sites=unresolved_sites,
            pyright_query_latency_ms=[latency_ms],
            pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
            findings=self._pop_findings(),
        )

    def resolve_references(
        self,
        file_path: str | Path,
        sites: Sequence[ReferenceSite],
    ) -> ReferenceResolutionResult:
        path = Path(file_path).resolve()
        index = self._function_index_for_path(path)
        reference_sites_total = len(sites)
        if index.parse_status == "syntax_error":
            return ReferenceResolutionResult(
                reference_sites_total=reference_sites_total,
                unresolved_reference_sites_total=reference_sites_total,
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
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
            )
        if not self._ensure_process():
            return ReferenceResolutionResult(
                reference_sites_total=reference_sites_total,
                unresolved_reference_sites_total=reference_sites_total,
                pyright_index_parse_latency_ms=self._pop_index_parse_latencies(),
                findings=self._pop_findings(),
            )

        deadline = self._deadline_for_file(path)
        latency_started = time.perf_counter()
        try:
            edges, resolved, skipped_external, unresolved = self._resolve_references_with_pyright(
                path,
                index,
                sites,
                deadline,
            )
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
        except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
            self._record_restart_or_poison(str(exc))
            edges = []
            resolved = 0
            skipped_external = 0
            unresolved = reference_sites_total
        finally:
            self._file_deadlines.pop(path, None)
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
        )

    def _resolve_with_pyright(
        self,
        path: Path,
        index: _FunctionIndex,
        functions: Sequence[_FunctionInfo],
        deadline: float,
    ) -> tuple[list[CallsRawEdge], int, list[UnresolvedCallSite]]:
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
            for function in functions:
                self._ensure_file_budget(deadline)
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
                    edges.append(edge)

                function_unresolved_sites = _unresolved_call_sites_for_function(
                    index,
                    function,
                    set(grouped),
                )
                unresolved_total += _unresolved_call_site_total_for_function(
                    function,
                    set(grouped),
                )
                unresolved_sites.extend(function_unresolved_sites)
            return edges, unresolved_total, unresolved_sites
        finally:
            self._notify("textDocument/didClose", {"textDocument": {"uri": uri}})

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
            resolved_total = 0
            skipped_external_total = 0
            unresolved_total = 0
            for site_index, site in enumerate(sites):
                if self._file_budget_expired(deadline):
                    unresolved_total += len(sites) - site_index
                    self._record_finding(
                        FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT,
                        "pyright reference query timed out: analyze_file budget",
                        method="analyze_file budget",
                    )
                    break
                cache_key = _reference_lookup_cache_key(site, source_bytes)
                cached = lookup_cache.get(cache_key)
                if cached is None:
                    try:
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
                        candidate_ids = _filter_relation_candidates(site, candidate_ids)
                    except LspTimeoutError as exc:
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
            self._notify("textDocument/didClose", {"textDocument": {"uri": uri}})

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

    def _deadline_for_file(self, path: Path) -> float:
        return self._file_deadlines.setdefault(
            path,
            time.monotonic() + self.file_timeout_secs,
        )

    def _budgeted_timeout(self, deadline: float) -> float:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            method = "analyze_file budget"
            raise LspTimeoutError(method)
        return min(self.call_timeout_secs, remaining)

    def _ensure_file_budget(self, deadline: float) -> None:
        if self._file_budget_expired(deadline):
            method = "analyze_file budget"
            raise LspTimeoutError(method)

    def _file_budget_expired(self, deadline: float) -> bool:
        return deadline - time.monotonic() <= 0

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
            return self._start_process()
        if self._process.poll() is None:
            return True
        self._process = None
        self._record_restart_or_poison("pyright subprocess exited")
        if self._run_state.disabled:
            return False
        return self._start_process()

    def _record_restart_or_poison(self, reason: str) -> None:
        self._run_state.restart_count += 1
        if self._run_state.restart_count > self.max_restarts_per_run:
            self._run_state.disabled = True
            self._record_finding(
                FINDING_PYRIGHT_POISON_FRAME,
                "pyright restart cap exceeded; skipping call resolution",
                restart_count=self._run_state.restart_count,
                reason=reason,
            )
            return
        self._record_finding(
            FINDING_PYRIGHT_RESTART,
            "pyright subprocess died and was restarted",
            restart_count=self._run_state.restart_count,
            reason=reason,
        )

    def _start_process(self) -> bool:
        executable = self._resolve_executable()
        if executable is None:
            self._run_state.disabled = True
            self._record_finding(
                FINDING_PYRIGHT_UNAVAILABLE,
                "pyright-langserver is not available",
                executable=self.executable,
            )
            return False
        if self.install_check is not None and not self.install_check(executable):
            self._run_state.disabled = True
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
            self._initialize()
        except LspTimeoutError:
            self._run_state.disabled = True
            self._record_finding(
                FINDING_PYRIGHT_INIT_TIMEOUT,
                "pyright initialize handshake timed out",
                timeout_secs=self.init_timeout_secs,
            )
            process.kill()
            process.wait(timeout=2)
            return False
        except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
            self._run_state.disabled = True
            self._record_finding(
                FINDING_PYRIGHT_UNAVAILABLE,
                "pyright initialize handshake failed",
                error=str(exc),
            )
            if process.poll() is None:
                process.kill()
                process.wait(timeout=2)
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
        self._record_finding(
            FINDING_PYRIGHT_INSTALL_FAILURE,
            "pyright-langserver failed to start",
            executable=executable,
            error=str(exc),
        )
        return False

    def _initialize(self) -> None:
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
            self.init_timeout_secs,
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
        while True:
            response = self._read_message(timeout_secs)
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
