"""Runtime-VAL manifest builder (ADR-010 §Decision 3 M3).

Extended under ADR-010 §H2 landing scope N1: the serialized manifest now carries
per-contract dispatch-site claims so the runs-row records not just *which*
contracts were active during run X but *which dispatch sites* each
contract implemented.

At orchestrator bootstrap the declaration-contract registry
(``EXPECTED_CONTRACT_SITES`` / ``registered_declaration_contracts``) and
the Tier-1 error registry (``TIER_1_ERRORS``) are both frozen. The M3
finding requires these to be serialized into the Landscape run-header so
an auditor can answer:

- "Which VAL contracts were in force during run X?"
- "Which dispatch sites did contract Y implement during run X?" (N1)
- "Was the ``can_drop_rows`` contract active during run X?" (time-series audit)
- "Are the TIER_1_ERRORS the same across runs X and Y?" (regression detection)

Shape:

    {
      "declaration_contracts": [
        {"name": "passes_through_input",
         "class_name": "PassThroughDeclarationContract",
         "class_module": "elspeth.engine.executors.pass_through",
         "dispatch_sites": ["batch_flush_check", "post_emission_check"],
         "implementation_hash": "sha256:..."},
        ...
      ],
      "expected_contract_sites": {
         "passes_through_input": ["batch_flush_check", "post_emission_check"],
         ...
      },
      "tier_1_errors": [...],
    }

Ordering is deterministic (name / class_name / site sort) so the
serialised form is stable and hashable for cross-run regression comparisons.
"""

from __future__ import annotations

import ast
import hashlib
import inspect
import json
import textwrap
from types import CodeType
from typing import Any

from elspeth.contracts.declaration_contracts import (
    EXPECTED_CONTRACT_SITES,
    DeclarationContract,
    contract_sites,
    declaration_registry_is_frozen,
    registered_declaration_contracts,
)
from elspeth.contracts.tier_registry import (
    _TIER_1_ERRORS_VIEW,
    FrameworkBugError,
    tier_1_reason,
    tier_registry_is_frozen,
)


def _json_hash(payload: object) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return f"sha256:{hashlib.sha256(encoded.encode('utf-8')).hexdigest()[:16]}"


def _normalize_code_constant(value: object) -> object:
    if isinstance(value, CodeType):
        return {"code": _normalize_code_object(value)}
    if isinstance(value, tuple):
        return {"tuple": [_normalize_code_constant(item) for item in value]}
    if isinstance(value, list):
        return {"list": [_normalize_code_constant(item) for item in value]}
    if isinstance(value, frozenset):
        normalized_items = [_normalize_code_constant(item) for item in value]
        return {"frozenset": sorted(normalized_items, key=lambda item: json.dumps(item, sort_keys=True, separators=(",", ":")))}
    if isinstance(value, bytes):
        return {"bytes": value.hex()}
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    return {"repr": repr(value), "type": type(value).__name__}


def _normalize_code_object(code: CodeType) -> dict[str, object]:
    return {
        "argcount": code.co_argcount,
        "posonlyargcount": code.co_posonlyargcount,
        "kwonlyargcount": code.co_kwonlyargcount,
        "nlocals": code.co_nlocals,
        "stacksize": code.co_stacksize,
        "flags": code.co_flags,
        "code": code.co_code.hex(),
        "consts": [_normalize_code_constant(value) for value in code.co_consts],
        "names": list(code.co_names),
        "varnames": list(code.co_varnames),
        "freevars": list(code.co_freevars),
        "cellvars": list(code.co_cellvars),
        "exceptiontable": code.co_exceptiontable.hex(),
    }


def _callable_implementation_hash(func: object) -> str:
    code = getattr(func, "__code__", None)
    if isinstance(code, CodeType):
        return _json_hash(
            {
                "code": _normalize_code_object(code),
                "defaults": _normalize_code_constant(getattr(func, "__defaults__", None)),
                "kwdefaults": _normalize_code_constant(getattr(func, "__kwdefaults__", None)),
                "annotations": _normalize_code_constant(getattr(func, "__annotations__", None)),
            }
        )
    return _json_hash(
        {
            "module": getattr(func, "__module__", None),
            "qualname": getattr(func, "__qualname__", None),
            "repr": repr(func),
        }
    )


def _strip_docstrings(node: ast.AST) -> None:
    for child in ast.walk(node):
        body = getattr(child, "body", None)
        if not isinstance(body, list) or not body:
            continue
        first = body[0]
        if not isinstance(first, ast.Expr):
            continue
        value = first.value
        if isinstance(value, ast.Constant) and isinstance(value.value, str):
            body.pop(0)


def _class_source_hash(cls: type[object]) -> str:
    try:
        source = textwrap.dedent(inspect.getsource(cls))
    except (OSError, TypeError) as exc:
        raise FrameworkBugError(
            "Runtime-VAL manifest cannot hash source for "
            f"{cls.__module__}.{cls.__qualname__}: source unavailable. "
            "Classes recorded in the resume-trust manifest must be source-available."
        ) from exc
    tree = ast.parse(source)
    _strip_docstrings(tree)
    return _json_hash(ast.dump(tree, include_attributes=False))


def _unwrap_callable(attribute: object) -> object | None:
    if isinstance(attribute, staticmethod):
        return attribute.__func__
    if isinstance(attribute, classmethod):
        return attribute.__func__
    if isinstance(attribute, property):
        return attribute.fget
    if inspect.isfunction(attribute):
        return attribute
    return None


def _callable_dependency_key(func: object) -> str:
    return f"{getattr(func, '__module__', '<unknown>')}:{getattr(func, '__qualname__', repr(func))}"


def _is_runtime_val_helper_dependency(candidate: object, *, owner_module: str | None) -> bool:
    if not inspect.isfunction(candidate):
        return False
    module_name_obj: object = getattr(candidate, "__module__", None)
    if type(module_name_obj) is not str or module_name_obj == "builtins":
        return False
    module_name = module_name_obj
    return module_name == owner_module or module_name.startswith("elspeth.")


def _callable_dependency_hashes(func: object, *, seen: frozenset[str] = frozenset()) -> dict[str, object]:
    code = getattr(func, "__code__", None)
    globals_table = getattr(func, "__globals__", None)
    if not isinstance(code, CodeType) or not isinstance(globals_table, dict):
        return {}

    current_key = _callable_dependency_key(func)
    next_seen = seen | frozenset({current_key})
    owner_module = getattr(func, "__module__", None)
    dependencies: dict[str, object] = {}
    for name in sorted(code.co_names):
        if name not in globals_table:
            continue
        candidate = globals_table[name]
        if not _is_runtime_val_helper_dependency(candidate, owner_module=owner_module):
            continue
        dependency_key = _callable_dependency_key(candidate)
        if dependency_key in next_seen:
            continue
        dependencies[dependency_key] = {
            "implementation_hash": _callable_implementation_hash(candidate),
            "dependencies": _callable_dependency_hashes(candidate, seen=next_seen),
        }
    return dependencies


def _iter_relevant_method_hashes(cls: type[object], *, method_names: list[str] | None = None) -> dict[str, str]:
    if method_names is None:
        names = sorted(set(dir(cls)))
    else:
        names = sorted(set(method_names))

    method_hashes: dict[str, str] = {}
    for name in names:
        attribute = inspect.getattr_static(cls, name, None)
        callable_obj = _unwrap_callable(attribute)
        if callable_obj is None:
            continue
        module_name = getattr(callable_obj, "__module__", None)
        if module_name == "builtins":
            continue
        method_hashes[name] = _callable_implementation_hash(callable_obj)
    return method_hashes


def _iter_relevant_method_dependency_hashes(
    cls: type[object],
    *,
    method_names: list[str],
) -> dict[str, object]:
    dependency_hashes: dict[str, object] = {}
    for name in sorted(set(method_names)):
        attribute = inspect.getattr_static(cls, name, None)
        callable_obj = _unwrap_callable(attribute)
        if callable_obj is None:
            continue
        dependencies = _callable_dependency_hashes(callable_obj)
        if dependencies:
            dependency_hashes[name] = dependencies
    return dependency_hashes


def _payload_schema_hash(payload_schema: object) -> str:
    if not isinstance(payload_schema, type):
        return _json_hash({"repr": repr(payload_schema), "type": type(payload_schema).__name__})
    annotations = getattr(payload_schema, "__annotations__", {})
    return _json_hash(
        {
            "module": payload_schema.__module__,
            "qualname": payload_schema.__qualname__,
            "annotations": {key: repr(value) for key, value in sorted(annotations.items())},
            "required_keys": sorted(getattr(payload_schema, "__required_keys__", frozenset())),
            "optional_keys": sorted(getattr(payload_schema, "__optional_keys__", frozenset())),
            "source_hash": _class_source_hash(payload_schema),
        }
    )


def _class_implementation_hash(
    cls: type[object],
    *,
    method_names: list[str] | None = None,
    extra: dict[str, object] | None = None,
) -> str:
    mro_hashes = [
        {
            "module": base.__module__,
            "qualname": base.__qualname__,
            "source_hash": _class_source_hash(base),
        }
        for base in cls.mro()
        if base.__module__ != "builtins"
    ]
    return _json_hash(
        {
            "module": cls.__module__,
            "qualname": cls.__qualname__,
            "source_hash": _class_source_hash(cls),
            "mro": mro_hashes,
            "methods": _iter_relevant_method_hashes(cls, method_names=method_names),
            "extra": extra or {},
        }
    )


def _declaration_contract_implementation_hash(contract: DeclarationContract) -> str:
    cls = type(contract)
    method_names = ["applies_to", *sorted(contract_sites(contract))]
    return _class_implementation_hash(
        cls,
        method_names=method_names,
        extra={
            "payload_schema_hash": _payload_schema_hash(cls.payload_schema),
            "method_dependency_hashes": _iter_relevant_method_dependency_hashes(cls, method_names=method_names),
        },
    )


def _tier_1_implementation_hash(cls: type[BaseException]) -> str:
    return _class_implementation_hash(cls)


def _assert_runtime_val_registries_frozen() -> None:
    unfrozen: list[str] = []
    if not declaration_registry_is_frozen():
        unfrozen.append("declaration-contract registry")
    if not tier_registry_is_frozen():
        unfrozen.append("Tier-1 error registry")
    if unfrozen:
        raise FrameworkBugError(
            "build_runtime_val_manifest() requires frozen runtime-VAL registries. "
            f"Unfrozen: {', '.join(unfrozen)}. Call prepare_for_run() before serializing the run header."
        )


def build_runtime_val_manifest() -> dict[str, Any]:
    """Return a dict describing the runtime-VAL registries at call time."""
    _assert_runtime_val_registries_frozen()
    declarations = [
        {
            "name": contract.name,
            "class_name": type(contract).__name__,
            "class_module": type(contract).__module__,
            "dispatch_sites": sorted(contract_sites(contract)),
            "implementation_hash": _declaration_contract_implementation_hash(contract),
        }
        for contract in sorted(registered_declaration_contracts(), key=lambda c: c.name)
    ]
    tier_1_entries = [
        {
            "class_name": cls.__name__,
            "class_module": cls.__module__,
            "reason": tier_1_reason(cls),
            "implementation_hash": _tier_1_implementation_hash(cls),
        }
        for cls in sorted(_TIER_1_ERRORS_VIEW, key=lambda c: (c.__module__, c.__name__))
    ]
    expected_contract_sites_serialized: dict[str, list[str]] = {name: sorted(sites) for name, sites in EXPECTED_CONTRACT_SITES.items()}
    return {
        "declaration_contracts": declarations,
        "expected_contract_sites": expected_contract_sites_serialized,
        "tier_1_errors": tier_1_entries,
    }
