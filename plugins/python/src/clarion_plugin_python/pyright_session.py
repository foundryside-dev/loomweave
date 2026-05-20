from __future__ import annotations

import ast
import json
import math
import os
import select
import shutil
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import IO, TYPE_CHECKING, Any, Self
from urllib.parse import unquote, urlparse

from clarion_plugin_python import __version__
from clarion_plugin_python.call_resolver import (
    CallResolutionResult,
    CallsRawEdge,
    Finding,
    UnresolvedCallSite,
)
from clarion_plugin_python.entity_id import entity_id
from clarion_plugin_python.extractor import module_dotted_name
from clarion_plugin_python.qualname import reconstruct_qualname
from clarion_plugin_python.reference_resolver import (
    ReferenceResolutionResult,
    ReferenceSite,
    ReferencesRawEdge,
)

FINDING_PYRIGHT_RESTART = "CLA-PY-PYRIGHT-RESTART"
FINDING_PYRIGHT_POISON_FRAME = "CLA-PY-PYRIGHT-POISON-FRAME"
FINDING_PYRIGHT_INIT_TIMEOUT = "CLA-PY-PYRIGHT-INIT-TIMEOUT"
FINDING_PYRIGHT_UNAVAILABLE = "CLA-PY-PYRIGHT-UNAVAILABLE"
FINDING_PYRIGHT_INSTALL_FAILURE = "CLA-PY-PYRIGHT-INSTALL-FAILURE"
FINDING_PYRIGHT_CALL_RESOLUTION_TIMEOUT = "CLA-PY-CALL-RESOLUTION-TIMEOUT"
FINDING_PYRIGHT_REFERENCE_RESOLUTION_TIMEOUT = "CLA-PY-REFERENCE-RESOLUTION-TIMEOUT"
FINDING_PYRIGHT_REFERENCE_SITE_CAP = "CLA-PY-REFERENCE-SITE-CAP"

MAX_PYRIGHT_RESTARTS_PER_RUN = 3
MAX_REFERENCE_SITES_PER_FILE = 2000
PYRIGHT_INIT_TIMEOUT_SECS = 30.0
PYRIGHT_CALL_TIMEOUT_SECS = 5.0
PYRIGHT_FILE_TIMEOUT_SECS = 3.0
STDERR_TAIL_LIMIT = 65536
PYRIGHT_EXCLUDE_PATTERNS = [
    "**/.clarion/**",
    "**/.git/**",
    "**/.hg/**",
    "**/.svn/**",
    "**/.jj/**",
    "**/.venv/**",
    "**/__pycache__/**",
    "**/node_modules/**",
]
PROJECT_LOCAL_EXTERNAL_DIRS = {".clarion", ".git", ".hg", ".svn", ".jj", ".venv", "node_modules"}


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


@dataclass
class _ReferenceEdgeAccumulator:
    from_id: str
    to_id: str
    source_byte_start: int
    source_byte_end: int
    candidates: set[str]


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
        self._process: subprocess.Popen[bytes] | None = None
        self._stderr_thread: threading.Thread | None = None
        self._stderr_tail = bytearray()
        self._next_id = 1
        self._restart_count = 0
        self._disabled = False
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
                            if key is not None:
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
                unresolved_total += len(function_unresolved_sites)
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
            accumulators: dict[tuple[str, str], _ReferenceEdgeAccumulator] = {}
            lookup_cache: dict[tuple[str, str, str], tuple[list[str], bool]] = {}
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
        return self._target_ids_from_locations(result)

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

    def _target_ids_from_locations(self, result: object) -> tuple[list[str], bool]:
        locations = result if isinstance(result, list) else [result]
        candidate_ids: set[str] = set()
        saw_external = False
        for location in locations:
            target_id, external = self._target_id_from_location(location)
            if external:
                saw_external = True
            if target_id is not None:
                candidate_ids.add(target_id)
        return sorted(candidate_ids), saw_external

    def _target_id_from_location(self, location: object) -> tuple[str | None, bool]:
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
        key = _range_start_key(raw_range)
        if key is not None and key in target_index.entity_by_name_position:
            return target_index.entity_by_name_position[key], False
        return target_index.module_id, False

    def _ensure_process(self) -> bool:
        if self._disabled:
            return False
        if self._process is None:
            return self._start_process()
        if self._process.poll() is None:
            return True
        self._process = None
        self._record_restart_or_poison("pyright subprocess exited")
        if self._disabled:
            return False
        return self._start_process()

    def _record_restart_or_poison(self, reason: str) -> None:
        self._restart_count += 1
        if self._restart_count > self.max_restarts_per_run:
            self._disabled = True
            self._record_finding(
                FINDING_PYRIGHT_POISON_FRAME,
                "pyright restart cap exceeded; skipping call resolution",
                restart_count=self._restart_count,
                reason=reason,
            )
            return
        self._record_finding(
            FINDING_PYRIGHT_RESTART,
            "pyright subprocess died and was restarted",
            restart_count=self._restart_count,
            reason=reason,
        )

    def _start_process(self) -> bool:
        executable = self._resolve_executable()
        if executable is None:
            self._disabled = True
            self._record_finding(
                FINDING_PYRIGHT_UNAVAILABLE,
                "pyright-langserver is not available",
                executable=self.executable,
            )
            return False
        if self.install_check is not None and not self.install_check(executable):
            self._disabled = True
            self._record_finding(
                FINDING_PYRIGHT_INSTALL_FAILURE,
                "pyright-langserver executability check failed",
                executable=executable,
            )
            return False

        try:
            process = subprocess.Popen(  # noqa: S603 - executable path comes from manifest/PATH.
                [executable, "--stdio"],
                cwd=self.project_root,
                env=self._subprocess_env(),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except OSError as exc:
            self._disabled = True
            self._record_finding(
                FINDING_PYRIGHT_INSTALL_FAILURE,
                "pyright-langserver failed to start",
                executable=executable,
                error=str(exc),
            )
            return False

        self._process = process
        self._start_stderr_drain(process)
        try:
            self._initialize()
        except LspTimeoutError:
            self._disabled = True
            self._record_finding(
                FINDING_PYRIGHT_INIT_TIMEOUT,
                "pyright initialize handshake timed out",
                timeout_secs=self.init_timeout_secs,
            )
            process.kill()
            process.wait(timeout=2)
            return False
        except (LspTransportClosedError, BrokenPipeError, OSError) as exc:
            self._disabled = True
            self._record_finding(
                FINDING_PYRIGHT_UNAVAILABLE,
                "pyright initialize handshake failed",
                error=str(exc),
            )
            if process.poll() is None:
                process.kill()
                process.wait(timeout=2)
            return False
        return True

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
                "clientInfo": {"name": "clarion-plugin-python", "version": __version__},
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
            if b":" not in line:
                message = f"malformed LSP header: {line!r}"
                raise LspTransportClosedError(message)
            name, value = line.decode("ascii").strip().split(":", 1)
            headers[name.lower()] = value.strip()
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
    tree = ast.parse(source)
    parse_latency_ms = max(1, math.ceil((time.perf_counter() - parse_started) * 1000))
    functions: list[_FunctionInfo] = []
    entities: list[_EntityInfo] = []
    source_lines = source.splitlines()
    _collect_entities(tree, [tree], dotted_module, source_lines, functions, entities)
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
    )


def _collect_entities(  # noqa: PLR0913 - keeps function/class indexes in one traversal.
    node: ast.AST,
    parents: list[ast.AST],
    dotted_module: str,
    source_lines: list[str],
    out: list[_FunctionInfo],
    out_entities: list[_EntityInfo],
) -> None:
    for child in ast.iter_child_nodes(node):
        match child:
            case ast.FunctionDef() | ast.AsyncFunctionDef():
                python_qualname = reconstruct_qualname(child, parents)
                qualified_name = f"{dotted_module}.{python_qualname}"
                line_text = (
                    source_lines[child.lineno - 1] if child.lineno <= len(source_lines) else ""
                )
                character = line_text.find(child.name)
                if character < 0:
                    character = child.col_offset
                entity = _EntityInfo(
                    entity_id=entity_id("python", "function", qualified_name),
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
                        end_character=child.end_col_offset or child.col_offset,
                        call_sites=tuple(_function_call_sites(child)),
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
                )
            case ast.ClassDef():
                python_qualname = reconstruct_qualname(child, parents)
                qualified_name = f"{dotted_module}.{python_qualname}"
                line_text = (
                    source_lines[child.lineno - 1] if child.lineno <= len(source_lines) else ""
                )
                character = line_text.find(child.name)
                if character < 0:
                    character = child.col_offset
                out_entities.append(
                    _EntityInfo(
                        entity_id=entity_id("python", "class", qualified_name),
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
                )
            case _:
                _collect_entities(
                    child,
                    [*parents, child],
                    dotted_module,
                    source_lines,
                    out,
                    out_entities,
                )


def _merge_reference_site(
    accumulators: dict[tuple[str, str], _ReferenceEdgeAccumulator],
    site: ReferenceSite,
    candidate_ids: Sequence[str],
) -> None:
    sorted_candidates = sorted(set(candidate_ids))
    to_id = sorted_candidates[0]
    key = (site.from_id, to_id)
    existing = accumulators.get(key)
    if existing is None:
        accumulators[key] = _ReferenceEdgeAccumulator(
            from_id=site.from_id,
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


def _reference_lookup_cache_key(
    site: ReferenceSite,
    source_bytes: bytes,
) -> tuple[str, str, str]:
    lexeme = source_bytes[site.source_byte_start : site.source_byte_end].decode("utf-8")
    return site.from_id, site.kind, lexeme


def _sorted_reference_accumulators(
    accumulators: dict[tuple[str, str], _ReferenceEdgeAccumulator],
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
        "kind": "references",
        "from_id": accumulator.from_id,
        "to_id": accumulator.to_id,
        "source_byte_start": accumulator.source_byte_start,
        "source_byte_end": accumulator.source_byte_end,
        "confidence": "resolved" if len(candidates) == 1 else "ambiguous",
    }
    if len(candidates) > 1:
        edge["properties"] = {"candidates": candidates}
    return edge


def _function_call_sites(node: ast.FunctionDef | ast.AsyncFunctionDef) -> list[_CallSite]:
    visitor = _CallSiteVisitor()
    for statement in node.body:
        visitor.visit(statement)
    return visitor.call_sites


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
    def __init__(self) -> None:
        self.call_sites: list[_CallSite] = []

    def visit_Call(self, node: ast.Call) -> None:
        func = node.func
        callee_expr = ast.unparse(func)
        self.call_sites.append(
            _CallSite(
                func.lineno - 1,
                func.col_offset,
                (func.end_lineno or func.lineno) - 1,
                func.end_col_offset or func.col_offset,
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
    visitor = _DictDispatchVisitor(candidate_maps)
    for statement in function.node.body:
        visitor.visit(statement)
    return visitor.dispatches


def _dunder_call_dispatches(
    index: _FunctionIndex,
    function: _FunctionInfo,
) -> dict[tuple[int, int, int, int], set[str]]:
    if not index.dunder_call_by_class:
        return {}
    visitor = _DunderCallDispatchVisitor(index.dunder_call_by_class)
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
    def __init__(self, candidate_maps: dict[str, set[str]]) -> None:
        self.candidate_maps = candidate_maps
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
                func.col_offset,
                (func.end_lineno or func.lineno) - 1,
                func.end_col_offset or func.col_offset,
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
    def __init__(self, dunder_call_by_class: dict[str, str]) -> None:
        self.dunder_call_by_class = dunder_call_by_class
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
                func.col_offset,
                (func.end_lineno or func.lineno) - 1,
                func.end_col_offset or func.col_offset,
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


def _position_to_byte(index: _FunctionIndex, line: int, character: int) -> int:
    if line >= len(index.line_starts):
        return len(index.source.encode("utf-8"))
    line_start = index.line_starts[line]
    line_text = index.source.splitlines(keepends=True)[line] if index.source else ""
    return line_start + len(line_text[:character].encode("utf-8"))


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
    for function in index.functions:
        if function.line <= line <= function.end_line and (
            line != function.line or character >= function.character
        ):
            return function.entity_id
    return None


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
