"""L0 helpers for declaration-contract plugin typing and role detection.

These helpers let L2 engine code distinguish source and sink plugin instances
without importing the L3 plugin base classes. They intentionally avoid
``isinstance()`` and instead walk the plugin class MRO plus the instance
namespace so inherited class-level declarations remain visible.

Plugin discovery and registration remain nominal via ``BaseSource`` /
``BaseSink`` in ``plugins.infrastructure.base``. This module exists only for
runtime boundary contracts that need a lower-layer role check.
"""

from __future__ import annotations

from typing import Protocol, cast, runtime_checkable


@runtime_checkable
class ContractablePlugin(Protocol):
    """Minimal plugin surface the declaration contracts depend on."""

    name: str
    node_id: str | None


@runtime_checkable
class DeclaredOutputFieldsPlugin(ContractablePlugin, Protocol):
    declared_output_fields: frozenset[str]


@runtime_checkable
class DeclaredInputFieldsPlugin(ContractablePlugin, Protocol):
    declared_input_fields: frozenset[str]
    is_batch_aware: bool


def _validated_string_frozenset(
    value: object,
    *,
    owner_name: str,
    attr_name: str,
) -> frozenset[str]:
    if type(value) is not frozenset:
        raise TypeError(f"{owner_name}.{attr_name} must be frozenset, got {type(value).__name__!r}.")
    if any(type(item) is not str for item in value):
        raise TypeError(f"{owner_name}.{attr_name} must contain only str items.")
    return frozenset(value)


def _require_contractable_plugin(plugin: object) -> None:
    typed_plugin = cast(ContractablePlugin, plugin)
    name = typed_plugin.name
    if type(name) is not str or not name:
        raise TypeError(f"{type(plugin).__name__}.name must be a non-empty str.")
    node_id = typed_plugin.node_id
    if node_id is not None and type(node_id) is not str:
        raise TypeError(f"{type(plugin).__name__}.node_id must be str | None, got {type(node_id).__name__!r}.")


def _declared_frozenset_from_instance_or_mro(
    plugin: object,
    *,
    attr_name: str,
) -> frozenset[str] | None:
    instance_namespace = vars(plugin)
    if attr_name in instance_namespace:
        value = instance_namespace[attr_name]
        return _validated_string_frozenset(
            value,
            owner_name=type(plugin).__name__,
            attr_name=attr_name,
        )

    for owner in type(plugin).__mro__:
        namespace = vars(owner)
        if attr_name not in namespace:
            continue
        value = namespace[attr_name]
        return _validated_string_frozenset(
            value,
            owner_name=owner.__name__,
            attr_name=attr_name,
        )
    return None


def _class_mro_defines_method(plugin: object, method_name: str) -> bool:
    return any(method_name in vars(owner) for owner in type(plugin).__mro__)


def source_declared_guaranteed_fields(plugin: object) -> frozenset[str] | None:
    """Return source guarantees for real source-role plugins, else ``None``."""

    if not _class_mro_defines_method(plugin, "load"):
        return None
    return _declared_frozenset_from_instance_or_mro(
        plugin,
        attr_name="declared_guaranteed_fields",
    )


def sink_declared_required_fields(plugin: object) -> frozenset[str] | None:
    """Return sink requirements for real sink-role plugins, else ``None``."""

    if not _class_mro_defines_method(plugin, "write"):
        return None
    if not _class_mro_defines_method(plugin, "flush"):
        return None
    return _declared_frozenset_from_instance_or_mro(
        plugin,
        attr_name="declared_required_fields",
    )


def require_declared_output_fields_plugin(plugin: object) -> DeclaredOutputFieldsPlugin:
    """Return a runtime-validated plugin exposing ``declared_output_fields``."""

    typed_plugin = cast(DeclaredOutputFieldsPlugin, plugin)
    _validated_string_frozenset(
        typed_plugin.declared_output_fields,
        owner_name=type(plugin).__name__,
        attr_name="declared_output_fields",
    )
    _require_contractable_plugin(plugin)
    return typed_plugin


def require_declared_input_fields_plugin(plugin: object) -> DeclaredInputFieldsPlugin:
    """Return a runtime-validated plugin exposing ADR-013 input declarations."""

    typed_plugin = cast(DeclaredInputFieldsPlugin, plugin)
    _validated_string_frozenset(
        typed_plugin.declared_input_fields,
        owner_name=type(plugin).__name__,
        attr_name="declared_input_fields",
    )
    is_batch_aware = typed_plugin.is_batch_aware
    if type(is_batch_aware) is not bool:
        raise TypeError(f"{type(plugin).__name__}.is_batch_aware must be bool, got {type(is_batch_aware).__name__!r}.")
    _require_contractable_plugin(plugin)
    return typed_plugin
