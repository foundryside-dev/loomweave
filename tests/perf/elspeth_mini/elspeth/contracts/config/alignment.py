"""Field alignment documentation for Settings -> Runtime mapping.

This module provides machine-readable documentation of how Settings fields
map to runtime config. Used by:

1. AST checker - Verify all Settings fields are documented
2. Tests - Verify mappings are accurate
3. Humans - Understand the field flow

A field-orphaning bug (exponential_base orphaned) motivated this system.
By making the mapping explicit and checkable, we prevent future orphaning.

Categories:
- FIELD_MAPPINGS: Settings field -> Runtime field (when names differ)
- SETTINGS_TO_RUNTIME: Settings class -> Runtime class mapping
- EXEMPT_SETTINGS: Settings classes that don't need Runtime counterparts
- RUNTIME_TO_SUBSYSTEM: Runtime class -> INTERNAL_DEFAULTS subsystem key
"""

from types import MappingProxyType
from typing import Final

# =============================================================================
# FIELD_MAPPINGS - Where Settings and Runtime field names differ
# =============================================================================
#
# Format: {SettingsClass: {settings_field: runtime_field}}
# Only list fields that have DIFFERENT names. Same-name fields are implicit.
#
# Example: RetrySettings.initial_delay_seconds -> RetryConfig.base_delay
#          The names differ, so it must be documented here.

FIELD_MAPPINGS: Final[MappingProxyType[str, MappingProxyType[str, str]]] = MappingProxyType(
    {
        "RetrySettings": MappingProxyType(
            {
                "initial_delay_seconds": "base_delay",
                "max_delay_seconds": "max_delay",
            }
        ),
        "TelemetrySettings": MappingProxyType(
            {
                "exporters": "exporter_configs",
            }
        ),
        # RateLimitSettings, ConcurrencySettings, CheckpointSettings
        # all use same field names in Settings and Runtime
    }
)


# =============================================================================
# SETTINGS_TO_RUNTIME - Which Runtime class implements each Settings class
# =============================================================================
#
# Format: {SettingsClassName: RuntimeClassName}
# This documents the intended pairing for protocol verification.

SETTINGS_TO_RUNTIME: Final[MappingProxyType[str, str]] = MappingProxyType(
    {
        "RetrySettings": "RuntimeRetryConfig",
        "RateLimitSettings": "RuntimeRateLimitConfig",
        "ConcurrencySettings": "RuntimeConcurrencyConfig",
        "CheckpointSettings": "RuntimeCheckpointConfig",
        "TelemetrySettings": "RuntimeTelemetryConfig",
    }
)


# =============================================================================
# EXEMPT_SETTINGS - Settings classes that DON'T need Runtime counterparts
# =============================================================================
#
# These Settings classes are passed directly to components or used for
# validation only. They don't have a separate "Runtime*Config" class.
#
# Categories:
# - Plugin settings: Passed to plugin constructors as-is
# - Container settings: Top-level grouping, not a runtime concept
# - Infrastructure settings: Passed to infrastructure components directly
# - Config-driven settings: Used at DAG construction, not runtime

EXEMPT_SETTINGS: Final[frozenset[str]] = frozenset(
    {
        # Plugin option containers - passed to plugin __init__
        "SourceSettings",
        "TransformSettings",
        "SinkSettings",
        # Config-driven DAG construction - not runtime behavior
        "AggregationSettings",
        "GateSettings",
        "CoalesceSettings",
        "TriggerConfig",
        # Infrastructure - passed to components directly
        "DatabaseSettings",
        "LandscapeSettings",
        "LandscapeExportSettings",
        "PayloadStoreSettings",
        # Nested in RateLimitSettings - handled by parent
        "ServiceRateLimit",
        # Nested in TelemetrySettings - no Runtime counterpart
        "ExporterSettings",
        # Top-level container
        "ElspethSettings",
    }
)


# =============================================================================
# RUNTIME_TO_SUBSYSTEM - Which INTERNAL_DEFAULTS subsystem each Runtime uses
# =============================================================================
#
# Format: {RuntimeClassName: subsystem_key}
# Maps Runtime*Config classes to their INTERNAL_DEFAULTS subsystem key.
#
# Used by the AST checker to validate that hardcoded literals in from_settings()
# methods are documented in INTERNAL_DEFAULTS[subsystem][field].
#
# Only classes with hardcoded internal defaults need entries here.
# Classes that only use settings.X values don't need a mapping.

RUNTIME_TO_SUBSYSTEM: Final[MappingProxyType[str, str]] = MappingProxyType(
    {
        "RuntimeRetryConfig": "retry",
    }
)


def get_runtime_field_name(settings_class: str, settings_field: str) -> str:
    """Get the runtime field name for a settings field.

    Most fields have the same name in Settings and Runtime. Only fields
    listed in FIELD_MAPPINGS have different names.

    Args:
        settings_class: Name of the Settings class (e.g., "RetrySettings")
        settings_field: Field name in Settings (e.g., "initial_delay_seconds")

    Returns:
        Runtime field name (e.g., "base_delay") or same name if no mapping
    """
    # Only some classes have field renames; most use same names
    if settings_class in FIELD_MAPPINGS:
        class_mappings = FIELD_MAPPINGS[settings_class]
        if settings_field in class_mappings:
            return class_mappings[settings_field]
    # No mapping = same name (the common case)
    return settings_field


def get_settings_field_name(settings_class: str, runtime_field: str) -> str:
    """Get the settings field name for a runtime field (reverse lookup).

    Args:
        settings_class: Name of the Settings class
        runtime_field: Field name in Runtime config

    Returns:
        Settings field name or same name if no mapping
    """
    # Only some classes have field renames; most use same names
    if settings_class in FIELD_MAPPINGS:
        class_mappings = FIELD_MAPPINGS[settings_class]
        # Reverse lookup through the mapping
        for settings_name, mapped_runtime in class_mappings.items():
            if mapped_runtime == runtime_field:
                return settings_name
    # No mapping = same name (the common case)
    return runtime_field


def is_exempt_settings(settings_class: str) -> bool:
    """Check if a Settings class is exempt from Runtime pairing.

    Args:
        settings_class: Name of the Settings class

    Returns:
        True if exempt (no Runtime counterpart expected)
    """
    return settings_class in EXEMPT_SETTINGS
