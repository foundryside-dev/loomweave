"""AST → entity extractor for the Python plugin (Sprint 2 / B.2).

Walks a parsed Python file and emits one ``module`` entity per file plus
one ``function`` entity per ``FunctionDef`` / ``AsyncFunctionDef`` and one
``class`` entity per ``ClassDef``. It also emits anchored scan-time
``imports``, ``calls``, and ``references`` candidate edges.

Entity shape matches the Rust host's ``RawEntity`` + ``RawSource``
contract (``crates/loomweave-core/src/plugin/host.rs:132-154``)::

    {
        "id": "python:function:...",
        "kind": "function",
        "qualified_name": "pkg.module.func",
        "source": {
            "file_path": "pkg/module.py",
            "source_range": {
                "start_line": 1, "start_col": 0,
                "end_line": 3, "end_col": 4,
            },
        },
    }

``source.file_path`` lands in the host's path jail (canonicalised +
checked against ``project_root``); any other source-side fields flow
through ``RawSource.extra`` (serde flatten) and are bounded by
``MAX_ENTITY_EXTRA_BYTES`` (64 KiB). ``qualified_name`` is the dotted
module prefix joined to Python's own ``__qualname__`` (reconstructed
per L7). The file_path passed on the wire may be absolute (what the
host sent) while the prefix used for qualified-name dotting can be the
relativised form — the two are decoupled via ``extract``'s
``module_prefix_path`` kwarg.

Behaviour (B.2 §3 Q1 supersedes Sprint-1 UQ-WP3-11 for module entities):

- Every analyzed file produces exactly one ``module`` entity. Empty
  files and comment-only files emit one with ``parse_status="ok"``.
  Zero *function* entities for empty files still holds (UQ-WP3-11).
- ``SyntaxError`` during ``ast.parse`` → one degraded module entity
  with ``parse_status="syntax_error"`` plus one stderr log line
  (UQ-WP3-02). The run continues; WP4-era findings can later attach a
  ``LMWV-PY-SYNTAX-ERROR`` annotation.
- Top-level ``__init__.py`` (where the dotted module name resolves to
  ``""``) is skipped with stderr; the entity-ID assembler rejects an
  empty ``canonical_qualified_name``.
- Paths starting with ``src/`` have the prefix stripped (UQ-WP3-05).
- ``pkg/__init__.py`` files yield qualified_names rooted at ``pkg``
  (not ``pkg.__init__``) — UQ-WP3-06.

Module-entity ``source_range`` is a whole-file cover with ``end_col=0``
as a sentinel for module entities only — class and function entities
carry real ``ast.*.end_col_offset`` data, so consumers must NOT infer
column semantics by analogy across kinds.
"""

from __future__ import annotations

import ast
import sys
import time
from dataclasses import dataclass, field
from pathlib import PurePosixPath
from typing import TYPE_CHECKING, Literal, NotRequired, TypedDict, cast

from loomweave_plugin_python.call_resolver import (
    CallResolutionResult,
    CallResolver,
    CallsEdgeProperties,
    Finding,
    NoOpCallResolver,
    UnresolvedCallSite,
)
from loomweave_plugin_python.entity_id import entity_id
from loomweave_plugin_python.qualname import reconstruct_qualname
from loomweave_plugin_python.reference_resolver import (
    NoOpReferenceResolver,
    ReferenceResolutionResult,
    ReferenceResolver,
    ReferencesEdgeProperties,
    ReferenceSite,
)

if TYPE_CHECKING:
    from loomweave_plugin_python.wardline_descriptor import WardlineVocabulary

_PLUGIN_ID = "python"
_NOOP_CALL_RESOLVER = NoOpCallResolver()
_NOOP_REFERENCE_RESOLVER = NoOpReferenceResolver()


class SourceRange(TypedDict):
    start_line: int
    start_col: int
    end_line: int
    end_col: int


class EntitySource(TypedDict):
    file_path: str
    source_range: SourceRange


class DefinitionSpan(TypedDict):
    """Sub-ranges within a function/class entity (clarion-460def6a51).

    ``decl_line`` is the line of the ``def``/``class`` keyword (Python 3.8+
    ``node.lineno``). ``body_line_start`` is the line of the first body
    statement (a docstring counts). The two ``decorator_*`` keys are present
    only when the entity is decorated; ``decorator_line_start`` is the
    topmost decorator line and matches the entity's expanded
    ``source_range.start_line``. They let a reader explain *why* a given
    line resolved to this entity (decorator vs declaration vs body) without
    re-reading source.
    """

    decl_line: int
    body_line_start: NotRequired[int]
    decorator_line_start: NotRequired[int]
    decorator_line_end: NotRequired[int]


# SEI signature schema version (ADR-038 REQ-C-01). Mirrors plugin.toml's
# `[signature] schema_version`; bumped when a per-kind shape changes
# incompatibly. The value is stamped into every emitted signature's ``v`` field
# so a consumer can detect a version change as a changed signature.
SIGNATURE_SCHEMA_VERSION = 1


class FunctionSignature(TypedDict):
    """SEI signature for a ``function`` entity (ADR-038 REQ-C-01).

    ``params`` is the ordered parameter list rendered as ``name`` or
    ``name: annotation`` strings (positional-only, positional, ``*args``,
    keyword-only, ``**kwargs`` in source order). ``return_ann`` is the unparsed
    return annotation or ``None``.
    """

    v: int
    params: list[str]
    return_ann: str | None


class ClassSignature(TypedDict):
    """SEI signature for a ``class`` entity (ADR-038 REQ-C-01): the unparsed
    base-class expressions in declaration order."""

    v: int
    bases: list[str]


class WardlineDecoratorMetadata(TypedDict):
    canonical_name: str
    qualified_name: str
    group: int
    attrs: dict[str, str]
    line: int


class WardlineEntityMetadata(TypedDict):
    descriptor_version: str
    confidence_basis: Literal["descriptor", "descriptor_version_skew"]
    decorators: list[WardlineDecoratorMetadata]


class RawEntity(TypedDict):
    """Wire shape matching the Rust host's RawEntity contract.

    ``parse_status`` is set on module entities only and rides through the
    host's ``serde(flatten) extra`` map. Class and function entities omit
    it; the field is ``NotRequired`` to keep mypy --strict happy.

    ``parent_id`` is a B.3 addition (ADR-026 decision 2): the dual-encoded
    half of the parent/contains relationship. Omitted entirely for module
    entities (they have no parent within the file); set on every
    function/class entity.
    """

    id: str
    kind: str  # "function" | "class" | "module"; not narrowed to keep extension cheap.
    qualified_name: str
    source: EntitySource
    parent_id: NotRequired[str]
    parse_status: NotRequired[Literal["ok", "syntax_error"]]
    # entity_context evidence (clarion-460def6a51). Set on function/class
    # entities; omitted for modules. Rides the host's RawEntity `extra` flatten
    # into `properties_json`, so no host or storage schema change is needed.
    definition: NotRequired[DefinitionSpan]
    # SEI signature (ADR-038 REQ-C-01 / Wave 1). A plugin-declared, versioned
    # JSON object the core stores verbatim and compares by string equality as
    # the matcher's move-case input. Set on function/class entities; omitted for
    # modules (the move case abstains — fail closed). Typed top-level field on
    # the host's RawEntity, not routed through `extra`.
    signature: NotRequired[FunctionSignature | ClassSignature]
    # WS5b catalogue/reachability categorisations. Typed top-level because the
    # core denormalises these into `entity_tags`; unknown/empty means no signal.
    tags: NotRequired[list[str]]
    # Short natural-language text used by analyze-time semantic embeddings.
    docstring: NotRequired[str]
    # Wardline descriptor-backed source-observed decorator facts. Wardline owns
    # the vocabulary; Loomweave stores only the annotation facts seen on entities.
    wardline: NotRequired[WardlineEntityMetadata]


class RawEdge(TypedDict):
    """Wire shape matching the Rust host's RawEdge contract (B.3 / ADR-026).

    Source range fields are NotRequired and omitted entirely for structural
    kinds (``contains``); anchored kinds (``calls``, etc.) include them when
    the language reaches that part of the ontology in later sprints.
    """

    kind: str
    from_id: str
    to_id: str
    source_byte_start: NotRequired[int]
    source_byte_end: NotRequired[int]
    confidence: NotRequired[Literal["resolved", "ambiguous", "inferred"]]
    properties: NotRequired[CallsEdgeProperties | ReferencesEdgeProperties | ImportsEdgeProperties]


class ImportsEdgeProperties(TypedDict):
    imported_name: str
    import_style: Literal["import", "from_import"]
    level: int
    type_only: NotRequired[bool]
    scope: NotRequired[Literal["function"]]


@dataclass
class ExtractionStats:
    unresolved_call_sites_total: int = 0
    unresolved_call_sites: list[UnresolvedCallSite] = field(default_factory=list)
    reference_sites_total: int = 0
    references_resolved_total: int = 0
    references_skipped_external_total: int = 0
    references_skipped_cap_total: int = 0
    unresolved_reference_sites_total: int = 0
    pyright_query_latency_ms: list[int] = field(default_factory=list)
    pyright_index_parse_latency_ms: list[int] = field(default_factory=list)
    extractor_parse_latency_ms: int = 0
    findings: list[Finding] = field(default_factory=list)
    duplicate_entities_dropped_total: int = 0

    @classmethod
    def from_resolution_results(
        cls,
        calls: CallResolutionResult,
        references: ReferenceResolutionResult,
    ) -> ExtractionStats:
        return cls(
            unresolved_call_sites_total=calls.unresolved_call_sites_total,
            unresolved_call_sites=calls.unresolved_call_sites,
            reference_sites_total=references.reference_sites_total,
            references_resolved_total=references.references_resolved_total,
            references_skipped_external_total=references.references_skipped_external_total,
            references_skipped_cap_total=references.references_skipped_cap_total,
            unresolved_reference_sites_total=references.unresolved_reference_sites_total,
            pyright_query_latency_ms=[
                *calls.pyright_query_latency_ms,
                *references.pyright_query_latency_ms,
            ],
            pyright_index_parse_latency_ms=[
                *calls.pyright_index_parse_latency_ms,
                *references.pyright_index_parse_latency_ms,
            ],
            findings=[*calls.findings, *references.findings],
        )


@dataclass
class ExtractResult:
    entities: list[RawEntity]
    edges: list[RawEdge]
    stats: ExtractionStats


def _module_source_range(source: str) -> SourceRange:
    """Whole-file cover for module entities (Q4 resolution, B.2 §3 Q4).

    Uniform formula regardless of ``parse_status``: ``end_line =
    source.count('\\n') + 1``, ``end_col = 0``. The ``end_col = 0`` value
    is a sentinel for module entities only — it means "end-of-file," NOT
    "column 0 of the last line." Class and function entities use real
    ``ast.*.end_col_offset`` data; consumers must not infer column
    semantics by analogy across kinds.
    """
    return {
        "start_line": 1,
        "start_col": 0,
        "end_line": source.count("\n") + 1,
        "end_col": 0,
    }


def module_dotted_name(module_path: str) -> str:
    """Derive the dotted module prefix from a root-relative source path.

    Rules:
    - Leading ``src/`` is stripped (UQ-WP3-05).
    - The ``.py`` suffix is dropped.
    - ``__init__`` filenames collapse to their containing package
      (UQ-WP3-06: ``pkg/__init__.py`` → ``pkg``).
    - Path separators become ``.``.

    ``module_path`` itself remains unchanged; it's stored on the entity
    as a property so WP4 can still find the file on disk.
    """
    parts = list(PurePosixPath(module_path).parts)
    if parts and parts[0] == "src":
        parts = parts[1:]
    if parts:
        last = parts[-1]
        if last.endswith(".py"):
            stem = last[:-3]
            if stem == "__init__":
                parts = parts[:-1]
            else:
                parts[-1] = stem
    return ".".join(parts)


def _build_module_entity(
    source: str,
    dotted_module: str,
    file_path: str,
    parse_status: Literal["ok", "syntax_error"],
    docstring: str | None = None,
) -> RawEntity:
    """Build the per-file module entity (Q1 + Q4 resolutions)."""
    entity: RawEntity = {
        "id": entity_id(_PLUGIN_ID, "module", dotted_module),
        "kind": "module",
        "qualified_name": dotted_module,
        "source": {
            "file_path": file_path,
            "source_range": _module_source_range(source),
        },
        "parse_status": parse_status,
    }
    _attach_optional_entity_metadata(entity, docstring=docstring, tags=[])
    return entity


def extract(  # noqa: PLR0913 - resolver seams + optional Wardline vocabulary are caller-owned.
    source: str,
    file_path: str,
    *,
    module_prefix_path: str | None = None,
    call_resolver: CallResolver = _NOOP_CALL_RESOLVER,
    reference_resolver: ReferenceResolver = _NOOP_REFERENCE_RESOLVER,
    wardline_vocabulary: WardlineVocabulary | None = None,
) -> tuple[list[RawEntity], list[RawEdge]]:
    result = extract_with_stats(
        source,
        file_path,
        module_prefix_path=module_prefix_path,
        call_resolver=call_resolver,
        reference_resolver=reference_resolver,
        wardline_vocabulary=wardline_vocabulary,
    )
    return result.entities, result.edges


def extract_with_stats(  # noqa: PLR0913 - resolver seams + optional Wardline vocabulary are caller-owned.
    source: str,
    file_path: str,
    *,
    module_prefix_path: str | None = None,
    call_resolver: CallResolver = _NOOP_CALL_RESOLVER,
    reference_resolver: ReferenceResolver = _NOOP_REFERENCE_RESOLVER,
    wardline_vocabulary: WardlineVocabulary | None = None,
) -> ExtractResult:
    """Return extracted entities/edges plus resolver observability stats.

    Always emits exactly one module entity (B.2 Q1) prepended to the
    entity list; functions and classes follow. B.3 also emits one
    ``contains`` edge per non-module entity (immediate-parent → child),
    plus a ``parent_id`` field on each non-module entity (the dual
    encoding from ADR-026 decision 2). Module entities have no parent
    within the file, so they omit ``parent_id`` and have no contains edge.

    ``file_path`` lands in each entity's ``source.file_path`` verbatim.
    ``module_prefix_path`` (default: same as ``file_path``) is the path
    whose dotted form prefixes every entity's ``qualified_name`` —
    callers can supply a project-relative path here while keeping
    ``file_path`` absolute so the host's path jail validates the
    original path.

    Same-id collisions (PEP-484 ``@overload`` stubs, ``singledispatch``
    ``def _(...):`` sequences, intentional redefinitions) are resolved at
    the emit boundary so the host's ``UNIQUE(entities.id)`` never trips
    mid-run. ``@overload`` stubs are recognised and dropped *before* the
    walk descends — their bodies (``...``) carry only type-checker hints
    so signature references are also suppressed. Any other duplicates
    that survive (e.g. aliased ``from typing import overload as o``,
    ``singledispatch.register`` users writing ``def _():`` repeatedly)
    are deduplicated first-wins with a stderr line per drop and
    ``ExtractionStats.duplicate_entities_dropped_total`` bumped per drop.
    """
    prefix_source = module_prefix_path if module_prefix_path is not None else file_path
    dotted_module = module_dotted_name(prefix_source)
    is_package_module = PurePosixPath(prefix_source).name == "__init__.py"

    # Top-level __init__.py would resolve to "" — entity_id() rejects that
    # (crates/loomweave-core/src/entity_id.rs:97-101). Skip with stderr.
    if not dotted_module:
        sys.stderr.write(
            f"loomweave-plugin-python: skipping {file_path}: "
            f"top-level __init__.py has no package name\n",
        )
        return ExtractResult([], [], ExtractionStats())

    parse_started_ns = time.perf_counter_ns()
    try:
        tree = ast.parse(source)
    except SyntaxError as exc:
        parse_latency_ms = _elapsed_ms(parse_started_ns)
        sys.stderr.write(
            f"loomweave-plugin-python: skipping {file_path}: syntax error at "
            f"line {exc.lineno}: {exc.msg}\n",
        )
        return ExtractResult(
            [_build_module_entity(source, dotted_module, file_path, "syntax_error")],
            [],
            ExtractionStats(extractor_parse_latency_ms=parse_latency_ms),
        )
    parse_latency_ms = _elapsed_ms(parse_started_ns)

    module_entity = _build_module_entity(
        source, dotted_module, file_path, "ok", ast.get_docstring(tree)
    )
    entities: list[RawEntity] = [module_entity]
    edges: list[RawEdge] = []
    function_ids: list[str] = []
    walk_state = _WalkState(
        seen_ids={module_entity["id"]},
        file_path=file_path,
        exported_names=_module_export_names(tree),
        wardline_vocabulary=wardline_vocabulary,
    )
    _walk(
        tree,
        [tree],
        dotted_module,
        file_path,
        module_entity["id"],
        entities,
        edges,
        function_ids,
        walk_state,
    )
    edges.extend(
        _collect_import_edges(
            source,
            tree,
            dotted_module,
            module_entity["id"],
            is_package_module=is_package_module,
        ),
    )
    reference_sites = _collect_reference_sites(source, tree, dotted_module, module_entity["id"])
    call_stats = call_resolver.resolve_calls(file_path, function_ids)
    reference_stats = reference_resolver.resolve_references(file_path, reference_sites)
    edges.extend(cast("list[RawEdge]", call_stats.edges))
    edges.extend(cast("list[RawEdge]", reference_stats.edges))
    stats = ExtractionStats.from_resolution_results(call_stats, reference_stats)
    stats.extractor_parse_latency_ms = parse_latency_ms
    stats.duplicate_entities_dropped_total = walk_state.duplicate_entities_dropped
    return ExtractResult(entities, edges, stats)


def _elapsed_ms(started_ns: int) -> int:
    return max(1, (time.perf_counter_ns() - started_ns + 999_999) // 1_000_000)


def _collect_import_edges(
    source: str,
    tree: ast.Module,
    dotted_module: str,
    module_entity_id: str,
    *,
    is_package_module: bool,
) -> list[RawEdge]:
    collector = _ImportEdgeCollector(
        source,
        dotted_module,
        module_entity_id,
        is_package_module=is_package_module,
    )
    collector.visit(tree)
    return collector.edges


class _ImportEdgeCollector(ast.NodeVisitor):
    def __init__(
        self,
        source: str,
        dotted_module: str,
        module_entity_id: str,
        *,
        is_package_module: bool,
    ) -> None:
        self.source = source
        self.dotted_module = dotted_module
        self.module_entity_id = module_entity_id
        self.is_package_module = is_package_module
        self.edges: list[RawEdge] = []
        self._function_depth = 0
        self._type_only_depth = 0

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._function_depth += 1
        try:
            self.generic_visit(node)
        finally:
            self._function_depth -= 1

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._function_depth += 1
        try:
            self.generic_visit(node)
        finally:
            self._function_depth -= 1

    def visit_If(self, node: ast.If) -> None:
        if _is_type_checking_guard(node.test):
            self._type_only_depth += 1
            try:
                for child in node.body:
                    self.visit(child)
            finally:
                self._type_only_depth -= 1
            for child in node.orelse:
                self.visit(child)
            return
        self.generic_visit(node)

    def visit_Import(self, node: ast.Import) -> None:
        source_byte_start, source_byte_end = _node_byte_range(self.source, node)
        for alias in node.names:
            self.edges.append(
                self._edge(
                    target_module=alias.name,
                    imported_name=alias.name,
                    import_style="import",
                    level=0,
                    source_range=(source_byte_start, source_byte_end),
                ),
            )

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        source_byte_start, source_byte_end = _node_byte_range(self.source, node)
        for alias in node.names:
            target_module = _import_from_target(
                self.dotted_module,
                node.module,
                node.level,
                alias.name,
                is_package_module=self.is_package_module,
            )
            if target_module is None:
                continue
            self.edges.append(
                self._edge(
                    target_module=target_module,
                    imported_name=alias.name,
                    import_style="from_import",
                    level=node.level,
                    source_range=(source_byte_start, source_byte_end),
                ),
            )

    def _edge(
        self,
        *,
        target_module: str,
        imported_name: str,
        import_style: Literal["import", "from_import"],
        level: int,
        source_range: tuple[int, int],
    ) -> RawEdge:
        source_byte_start, source_byte_end = source_range
        properties: ImportsEdgeProperties = {
            "imported_name": imported_name,
            "import_style": import_style,
            "level": level,
        }
        if self._type_only_depth > 0:
            properties["type_only"] = True
        if self._function_depth > 0:
            properties["scope"] = "function"
        return {
            "kind": "imports",
            "from_id": self.module_entity_id,
            "to_id": entity_id(_PLUGIN_ID, "module", target_module),
            "source_byte_start": source_byte_start,
            "source_byte_end": source_byte_end,
            "confidence": "resolved",
            "properties": properties,
        }


def _is_type_checking_guard(expr: ast.expr) -> bool:
    if isinstance(expr, ast.Name):
        return expr.id == "TYPE_CHECKING"
    if isinstance(expr, ast.Attribute):
        return (
            expr.attr == "TYPE_CHECKING"
            and isinstance(expr.value, ast.Name)
            and expr.value.id == "typing"
        )
    if isinstance(expr, ast.BoolOp):
        if isinstance(expr.op, ast.And):
            return any(_is_type_checking_guard(value) for value in expr.values)
        if isinstance(expr.op, ast.Or):
            return all(_is_type_checking_guard(value) for value in expr.values)
    return False


def _import_from_target(
    dotted_module: str,
    module: str | None,
    level: int,
    imported_name: str,
    *,
    is_package_module: bool,
) -> str | None:
    if level == 0:
        return module

    base_parts = _relative_import_base_parts(
        dotted_module,
        level,
        is_package_module=is_package_module,
    )
    if base_parts is None:
        return None

    target_parts = [*base_parts]
    if module:
        target_parts.extend(part for part in module.split(".") if part)
    elif imported_name != "*":
        target_parts.append(imported_name)

    return ".".join(target_parts) if target_parts else None


def _relative_import_base_parts(
    dotted_module: str,
    level: int,
    *,
    is_package_module: bool,
) -> list[str] | None:
    all_parts = dotted_module.split(".")
    package_parts = all_parts if is_package_module else all_parts[:-1]
    keep = len(package_parts) - (level - 1)
    if keep < 0:
        return None
    return package_parts[:keep]


def _node_byte_range(source: str, node: ast.Import | ast.ImportFrom) -> tuple[int, int]:
    line_starts = _line_starts(source)
    start_line = node.lineno - 1
    end_line = (node.end_lineno or node.lineno) - 1
    end_col = node.end_col_offset if node.end_col_offset is not None else node.col_offset
    return line_starts[start_line] + node.col_offset, line_starts[end_line] + end_col


def _collect_reference_sites(
    source: str,
    tree: ast.Module,
    dotted_module: str,
    module_entity_id: str,
) -> list[ReferenceSite]:
    collector = _ReferenceSiteCollector(source, tree, dotted_module, module_entity_id)
    collector.visit(tree)
    return collector.sites


class _ReferenceSiteCollector(ast.NodeVisitor):
    def __init__(
        self,
        source: str,
        tree: ast.Module,
        dotted_module: str,
        module_entity_id: str,
    ) -> None:
        self.source = source
        self.source_lines = source.splitlines(keepends=True)
        self.line_starts = _line_starts(source)
        self.dotted_module = dotted_module
        self.parents: list[ast.AST] = [tree]
        self.owner_stack = [module_entity_id]
        self.bound_stack = [_scope_local_names(tree)]
        self.annotation_depth = 0
        self.sites: list[ReferenceSite] = []

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._visit_function(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._visit_function(node)

    def _visit_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
        # PEP 484 stub: signature annotations are type-checker hints, not
        # references the consult-mode briefing cares about; the body is `...`.
        # Skipping keeps reference-site ownership consistent with `_walk` (no
        # entity for the stub → nothing to attribute references to).
        if _has_overload_decorator(node):
            return
        function_id = self._entity_id_for_scope("function", node)
        self.owner_stack.append(function_id)
        self.bound_stack.append(_scope_local_names(node))
        self._visit_function_signature(node)
        self.parents.append(node)
        for statement in node.body:
            self.visit(statement)
        self.parents.pop()
        self.bound_stack.pop()
        self.owner_stack.pop()

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        class_id = self._entity_id_for_scope("class", node)
        self.owner_stack.append(class_id)
        self.bound_stack.append(_scope_local_names(node))
        self.parents.append(node)
        for statement in node.body:
            self.visit(statement)
        self.parents.pop()
        self.bound_stack.pop()
        self.owner_stack.pop()

    def visit_Lambda(self, node: ast.Lambda) -> None:
        # Lambdas are not entities in v0.1; keep the surrounding owner and
        # suppress lambda-local argument names.
        self.bound_stack.append(_lambda_bound_names(node))
        self.visit(node.body)
        self.bound_stack.pop()

    def visit_AnnAssign(self, node: ast.AnnAssign) -> None:
        self._visit_annotation(node.annotation)
        if node.value is not None:
            self.visit(node.value)

    def visit_arg(self, node: ast.arg) -> None:
        if node.annotation is not None:
            self._visit_annotation(node.annotation)

    def visit_Call(self, node: ast.Call) -> None:
        # `calls` owns the callee expression; references inside it are suppressed.
        for arg in node.args:
            self.visit(arg)
        for keyword in node.keywords:
            self.visit(keyword.value)

    def visit_Name(self, node: ast.Name) -> None:
        if isinstance(node.ctx, ast.Load) and not self._is_non_entity_local(node.id):
            self.sites.append(self._site_for_name(node))

    def _is_non_entity_local(self, name: str) -> bool:
        return any(name in scope for scope in self.bound_stack[1:])

    def _visit_function_signature(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
        for arg in [
            *node.args.posonlyargs,
            *node.args.args,
            *node.args.kwonlyargs,
        ]:
            self.visit(arg)
        if node.args.vararg is not None:
            self.visit(node.args.vararg)
        if node.args.kwarg is not None:
            self.visit(node.args.kwarg)
        if node.returns is not None:
            self._visit_annotation(node.returns)
        for default in [*node.args.defaults, *(d for d in node.args.kw_defaults if d is not None)]:
            self.visit(default)

    def _visit_annotation(self, node: ast.expr) -> None:
        self.annotation_depth += 1
        self.visit(node)
        self.annotation_depth -= 1

    def _entity_id_for_scope(
        self,
        kind: Literal["class", "function"],
        node: ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef,
    ) -> str:
        python_qualname = reconstruct_qualname(node, self.parents)
        qualified_name = (
            f"{self.dotted_module}.{python_qualname}" if self.dotted_module else python_qualname
        )
        return entity_id(_PLUGIN_ID, kind, qualified_name)

    def _site_for_name(self, node: ast.Name) -> ReferenceSite:
        line = node.lineno - 1
        end_line = (node.end_lineno or node.lineno) - 1
        end_col = node.end_col_offset or node.col_offset + len(node.id.encode("utf-8"))
        source_byte_start = self.line_starts[line] + node.col_offset
        source_byte_end = self.line_starts[end_line] + end_col
        return ReferenceSite(
            from_id=self.owner_stack[-1],
            line=line,
            character=_byte_col_to_lsp_character(self.source_lines[line], node.col_offset),
            end_line=end_line,
            end_character=_byte_col_to_lsp_character(self.source_lines[end_line], end_col),
            source_byte_start=source_byte_start,
            source_byte_end=source_byte_end,
            kind="annotation" if self.annotation_depth else "name",
        )


def _line_starts(source: str) -> tuple[int, ...]:
    starts = [0]
    total = 0
    for line in source.splitlines(keepends=True):
        total += len(line.encode("utf-8"))
        starts.append(total)
    return tuple(starts)


def _byte_col_to_lsp_character(line: str, byte_col: int) -> int:
    prefix = line.encode("utf-8")[:byte_col].decode("utf-8")
    return len(prefix.encode("utf-16-le")) // 2


def _scope_local_names(
    scope: ast.Module | ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef,
) -> set[str]:
    collector = _LocalNameCollector()
    if isinstance(scope, (ast.FunctionDef, ast.AsyncFunctionDef)):
        collector.names.update(_function_arg_names(scope))
    for statement in scope.body:
        collector.visit(statement)
    return collector.names


def _lambda_bound_names(node: ast.Lambda) -> set[str]:
    return set(_arguments_arg_names(node.args))


def _function_arg_names(node: ast.FunctionDef | ast.AsyncFunctionDef) -> set[str]:
    return set(_arguments_arg_names(node.args))


def _arguments_arg_names(args: ast.arguments) -> list[str]:
    names = [
        *(arg.arg for arg in args.posonlyargs),
        *(arg.arg for arg in args.args),
        *(arg.arg for arg in args.kwonlyargs),
    ]
    if args.vararg is not None:
        names.append(args.vararg.arg)
    if args.kwarg is not None:
        names.append(args.kwarg.arg)
    return names


class _LocalNameCollector(ast.NodeVisitor):
    def __init__(self) -> None:
        self.names: set[str] = set()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        _ = node

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        _ = node

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        _ = node

    def visit_Name(self, node: ast.Name) -> None:
        if isinstance(node.ctx, (ast.Store, ast.Del)):
            self.names.add(node.id)


@dataclass
class _WalkState:
    """Mutable accumulator threaded through ``_walk`` for cross-cutting bookkeeping.

    ``seen_ids`` is seeded with the module-entity id by ``extract_with_stats``
    so the safety net catches the (degenerate) case of a function colliding
    with the module's id. ``duplicate_entities_dropped`` is bumped once per
    same-id drop; the caller copies it into ``ExtractionStats``.
    """

    seen_ids: set[str]
    file_path: str
    wardline_vocabulary: WardlineVocabulary | None = None
    exported_names: set[str] = field(default_factory=set)
    duplicate_entities_dropped: int = 0


def _has_overload_decorator(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    """Return True if ``node`` is decorated with ``@overload`` (PEP 484 stub).

    Recognises three import-name forms in source: bare ``@overload`` (from
    ``from typing import overload``), ``@typing.overload``, and
    ``@typing_extensions.overload``. Aliased re-imports such as
    ``from typing import overload as o`` defeat this pattern-based check —
    the safety-net dedup in ``_walk`` catches the resulting same-id
    collision and keeps the run alive.
    """
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


def _walk(  # noqa: PLR0913 - recursive walker needs both accumulators + parent context (B.3)
    node: ast.AST,
    parents: list[ast.AST],
    dotted_module: str,
    file_path: str,
    parent_entity_id: str,
    out_entities: list[RawEntity],
    out_edges: list[RawEdge],
    out_function_ids: list[str],
    state: _WalkState,
) -> None:
    """Recursively walk ``node``'s AST children, emitting entities + contains edges.

    ``parent_entity_id`` is the immediate-parent entity id for direct
    children of ``node``. When a child entity is itself an entity-bearing
    node (Class/FunctionDef), recursion drops into it with the child's
    own id as the new parent — so grandchildren get the right ``from_id``
    on their contains edge (B.3 Q3: emitter is exhaustive, never
    transitive).

    Two stub-skip rules keep the host's ``UNIQUE(entities.id)`` from
    tripping. (1) ``@overload``-decorated functions are recognised
    semantically: skip emission, skip recursion (PEP 484 stub bodies are
    ``...``). The implementation appears last in source order and emits
    normally. (2) Any other surviving same-id collision (aliased
    ``overload`` imports, ``singledispatch.register`` ``def _():``
    sequences, manual redefinition) is dropped first-wins with a stderr
    line and a ``state.duplicate_entities_dropped`` bump. Recursion into
    the dropped child is suppressed too: its nested entities would carry
    a parent_id whose entity the host never sees.
    """
    for child in ast.iter_child_nodes(node):
        new_parent_id = parent_entity_id
        match child:
            case ast.FunctionDef() | ast.AsyncFunctionDef():
                if _has_overload_decorator(child):
                    continue
                entity, child_id = _build_function_entity(
                    child,
                    parents,
                    dotted_module,
                    parent_entity_id,
                    state,
                )
                if child_id in state.seen_ids:
                    state.duplicate_entities_dropped += 1
                    sys.stderr.write(
                        f"loomweave-plugin-python: dropping duplicate entity {child_id} "
                        f"in {state.file_path} at line {child.lineno} "
                        f"(first definition wins)\n",
                    )
                    continue
                state.seen_ids.add(child_id)
                out_entities.append(entity)
                out_edges.append(_contains_edge(parent_entity_id, child_id))
                out_function_ids.append(child_id)
                new_parent_id = child_id
            case ast.ClassDef():
                entity, child_id = _build_class_entity(
                    child,
                    parents,
                    dotted_module,
                    parent_entity_id,
                    state,
                )
                if child_id in state.seen_ids:
                    state.duplicate_entities_dropped += 1
                    sys.stderr.write(
                        f"loomweave-plugin-python: dropping duplicate entity {child_id} "
                        f"in {state.file_path} at line {child.lineno} "
                        f"(first definition wins)\n",
                    )
                    continue
                state.seen_ids.add(child_id)
                out_entities.append(entity)
                out_edges.append(_contains_edge(parent_entity_id, child_id))
                new_parent_id = child_id
        _walk(
            child,
            [*parents, child],
            dotted_module,
            file_path,
            new_parent_id,
            out_entities,
            out_edges,
            out_function_ids,
            state,
        )


def _definition_span(
    node: ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef,
) -> tuple[int, int, DefinitionSpan]:
    """Return ``(start_line, start_col, definition)`` for a definition node.

    ``node.lineno`` is the ``def``/``class`` keyword line (Python 3.8+);
    decorators sit above it. When the node is decorated the returned
    ``start_line``/``start_col`` extend the entity span up to the topmost
    decorator so an ``entity_at`` query on a decorator line resolves to this
    entity (clarion-460def6a51). The ``definition`` map records the
    sub-ranges that explain why a line matched.
    """
    definition: DefinitionSpan = {"decl_line": node.lineno}
    if node.body:
        definition["body_line_start"] = node.body[0].lineno
    start_line = node.lineno
    start_col = node.col_offset
    if node.decorator_list:
        topmost = min(node.decorator_list, key=lambda d: (d.lineno, d.col_offset))
        decorator_line_start = topmost.lineno
        decorator_line_end = max(
            (d.end_lineno if d.end_lineno is not None else d.lineno) for d in node.decorator_list
        )
        definition["decorator_line_start"] = decorator_line_start
        definition["decorator_line_end"] = decorator_line_end
        start_line = decorator_line_start
        start_col = topmost.col_offset
    return start_line, start_col, definition


def _contains_edge(parent_id: str, child_id: str) -> RawEdge:
    """Build a ``contains`` edge per ADR-026 decision 3 (no source range)."""
    return {
        "kind": "contains",
        "from_id": parent_id,
        "to_id": child_id,
    }


_HTTP_ROUTE_DECORATOR_NAMES = {
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "options",
    "head",
    "route",
    "websocket",
}
_CLI_DECORATOR_NAMES = {"command", "group", "callback"}
_DATA_MODEL_BASE_NAMES = {"BaseModel", "Model", "SQLModel", "TypedDict"}


def _attach_optional_entity_metadata(
    entity: RawEntity,
    *,
    docstring: str | None,
    tags: set[str] | list[str],
) -> None:
    if docstring:
        entity["docstring"] = docstring
    if tags:
        entity["tags"] = sorted(tags)


def _attach_wardline_entity_metadata(
    entity: RawEntity,
    node: ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef,
    tags: set[str],
    vocabulary: WardlineVocabulary | None,
) -> None:
    if vocabulary is None:
        return
    decorators: list[WardlineDecoratorMetadata] = []
    for decorator in node.decorator_list:
        qualified_name = _expr_qualified_name(decorator)
        if qualified_name is None:
            continue
        entry = vocabulary.entry_for_decorator(qualified_name)
        if entry is None:
            continue
        decorators.append(
            {
                "canonical_name": entry.canonical_name,
                "qualified_name": qualified_name,
                "group": entry.group,
                "attrs": dict(entry.attrs),
                "line": decorator.lineno,
            },
        )
        tags.update({"wardline", f"wardline:{entry.canonical_name}"})
    if decorators:
        entity["wardline"] = {
            "descriptor_version": vocabulary.version,
            "confidence_basis": vocabulary.confidence_basis,
            "decorators": decorators,
        }


def _module_export_names(tree: ast.Module) -> set[str]:
    exported: set[str] = set()
    for statement in tree.body:
        if not isinstance(statement, ast.Assign):
            continue
        if not any(
            isinstance(target, ast.Name) and target.id == "__all__" for target in statement.targets
        ):
            continue
        match statement.value:
            case ast.List(elts=elts) | ast.Tuple(elts=elts) | ast.Set(elts=elts):
                for elt in elts:
                    if isinstance(elt, ast.Constant) and isinstance(elt.value, str):
                        exported.add(elt.value)
    return exported


def _expr_qualified_name(expr: ast.expr) -> str | None:
    match expr:
        case ast.Call(func=func):
            return _expr_qualified_name(func)
        case ast.Name(id=name):
            return name
        case ast.Attribute(value=value, attr=attr):
            base = _expr_qualified_name(value)
            return f"{base}.{attr}" if base else attr
        case _:
            return None


def _decorator_names(
    node: ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef,
) -> list[str]:
    return [name for decorator in node.decorator_list if (name := _expr_qualified_name(decorator))]


def _last_name(name: str) -> str:
    return name.rsplit(".", 1)[-1]


def _is_module_level(parents: list[ast.AST]) -> bool:
    return len(parents) == 1


def _function_tags(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    parents: list[ast.AST],
    exported_names: set[str],
) -> set[str]:
    tags: set[str] = set()
    if _is_module_level(parents) and node.name == "main":
        tags.add("entry-point")
    if _is_module_level(parents) and node.name in exported_names:
        tags.add("exported-api")
    if node.name.startswith("test_") or any(
        isinstance(parent, ast.ClassDef) and parent.name.startswith("Test") for parent in parents
    ):
        tags.add("test")
    decorator_names = _decorator_names(node)
    if any(_last_name(name) in _HTTP_ROUTE_DECORATOR_NAMES for name in decorator_names):
        tags.update({"http-route", "framework-handler"})
    if any(_last_name(name) in _CLI_DECORATOR_NAMES for name in decorator_names):
        tags.update({"cli-command", "framework-handler"})
    return tags


def _class_tags(node: ast.ClassDef, parents: list[ast.AST], exported_names: set[str]) -> set[str]:
    tags: set[str] = set()
    if _is_module_level(parents) and node.name in exported_names:
        tags.add("exported-api")
    if node.name.startswith("Test"):
        tags.add("test")
    decorator_names = _decorator_names(node)
    base_names = [_expr_qualified_name(base) for base in node.bases]
    if any(_last_name(name) == "dataclass" for name in decorator_names) or any(
        name is not None and _last_name(name) in _DATA_MODEL_BASE_NAMES for name in base_names
    ):
        tags.add("data-model")
    return tags


def _annotation_str(node: ast.expr | None) -> str | None:
    """Unparse an annotation/expression node to its canonical source text, or
    ``None`` when absent. ``ast.unparse`` is deterministic for a given AST."""
    if node is None:
        return None
    return ast.unparse(node)


def _format_param(arg: ast.arg, prefix: str = "") -> str:
    """Render one parameter as ``name`` or ``name: annotation`` (``prefix`` is
    ``*`` / ``**`` for var-positional / var-keyword params)."""
    annotation = _annotation_str(arg.annotation)
    name = f"{prefix}{arg.arg}"
    return f"{name}: {annotation}" if annotation is not None else name


def _function_signature(node: ast.FunctionDef | ast.AsyncFunctionDef) -> FunctionSignature:
    """SEI signature for a function (ADR-038 REQ-C-01). Near-redundant for the
    v1 deterministic move case (a byte-identical body already implies an
    identical ``def`` line), carried for spec conformance + the fuzzy future."""
    args = node.args
    params: list[str] = [_format_param(arg) for arg in (*args.posonlyargs, *args.args)]
    if args.vararg is not None:
        params.append(_format_param(args.vararg, "*"))
    params.extend(_format_param(arg) for arg in args.kwonlyargs)
    if args.kwarg is not None:
        params.append(_format_param(args.kwarg, "**"))
    return {
        "v": SIGNATURE_SCHEMA_VERSION,
        "params": params,
        "return_ann": _annotation_str(node.returns),
    }


def _class_signature(node: ast.ClassDef) -> ClassSignature:
    """SEI signature for a class (ADR-038 REQ-C-01): unparsed base expressions."""
    bases = [text for text in (_annotation_str(base) for base in node.bases) if text is not None]
    return {"v": SIGNATURE_SCHEMA_VERSION, "bases": bases}


def _build_function_entity(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    parents: list[ast.AST],
    dotted_module: str,
    parent_entity_id: str,
    state: _WalkState,
) -> tuple[RawEntity, str]:
    python_qualname = reconstruct_qualname(node, parents)
    qualified_name = f"{dotted_module}.{python_qualname}" if dotted_module else python_qualname
    end_line = node.end_lineno if node.end_lineno is not None else node.lineno
    end_col = node.end_col_offset if node.end_col_offset is not None else node.col_offset
    start_line, start_col, definition = _definition_span(node)
    child_id = entity_id(_PLUGIN_ID, "function", qualified_name)
    entity: RawEntity = {
        "id": child_id,
        "kind": "function",
        "qualified_name": qualified_name,
        "source": {
            "file_path": state.file_path,
            "source_range": {
                "start_line": start_line,
                "start_col": start_col,
                "end_line": end_line,
                "end_col": end_col,
            },
        },
        "parent_id": parent_entity_id,
        "definition": definition,
        "signature": _function_signature(node),
    }
    tags = _function_tags(node, parents, state.exported_names)
    _attach_wardline_entity_metadata(entity, node, tags, state.wardline_vocabulary)
    _attach_optional_entity_metadata(
        entity,
        docstring=ast.get_docstring(node),
        tags=tags,
    )
    return entity, child_id


def _build_class_entity(
    node: ast.ClassDef,
    parents: list[ast.AST],
    dotted_module: str,
    parent_entity_id: str,
    state: _WalkState,
) -> tuple[RawEntity, str]:
    """Build a class entity. Uses real ast.end_lineno/end_col_offset (not the module sentinel).

    Class methods continue to emit as ``function`` entities (per
    detailed-design.md:67); no separate ``method`` kind. Nested classes
    nest in the qualname per ``reconstruct_qualname`` (no ``<locals>``
    between class names).
    """
    python_qualname = reconstruct_qualname(node, parents)
    qualified_name = f"{dotted_module}.{python_qualname}" if dotted_module else python_qualname
    end_line = node.end_lineno if node.end_lineno is not None else node.lineno
    end_col = node.end_col_offset if node.end_col_offset is not None else node.col_offset
    start_line, start_col, definition = _definition_span(node)
    child_id = entity_id(_PLUGIN_ID, "class", qualified_name)
    entity: RawEntity = {
        "id": child_id,
        "kind": "class",
        "qualified_name": qualified_name,
        "source": {
            "file_path": state.file_path,
            "source_range": {
                "start_line": start_line,
                "start_col": start_col,
                "end_line": end_line,
                "end_col": end_col,
            },
        },
        "parent_id": parent_entity_id,
        "definition": definition,
        "signature": _class_signature(node),
    }
    tags = _class_tags(node, parents, state.exported_names)
    _attach_wardline_entity_metadata(entity, node, tags, state.wardline_vocabulary)
    _attach_optional_entity_metadata(
        entity,
        docstring=ast.get_docstring(node),
        tags=tags,
    )
    return entity, child_id
