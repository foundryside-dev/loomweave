"""Configuration contracts subpackage.

This subpackage contains:
- Runtime protocols (protocols.py) - what engine components expect
- Runtime config dataclasses (runtime.py) - concrete implementations
- Default registries (defaults.py) - POLICY_DEFAULTS, INTERNAL_DEFAULTS
- Field alignment documentation (alignment.py) - Settings→Runtime mapping

NOTE: Settings classes (RetrySettings, ElspethSettings, etc.) are NOT here.
      Import them from elspeth.core.config to avoid breaking the leaf boundary.

Import patterns:
    # Settings classes (from core, not contracts!)
    from elspeth.core.config import RetrySettings, ElspethSettings

    # Runtime protocols (for type hints)
    from elspeth.contracts.config import RuntimeRetryProtocol

    # Runtime config (concrete implementations)
    from elspeth.contracts.config import RuntimeRetryConfig

    # Defaults (for runtime config construction)
    from elspeth.contracts.config import POLICY_DEFAULTS, INTERNAL_DEFAULTS

    # Alignment (for tooling/tests)
    from elspeth.contracts.config import FIELD_MAPPINGS, EXEMPT_SETTINGS
"""

# =============================================================================
# Field alignment documentation
# =============================================================================
# Machine-readable mapping of Settings -> Runtime field names
from elspeth.contracts.config.alignment import (
    EXEMPT_SETTINGS,
    FIELD_MAPPINGS,
    SETTINGS_TO_RUNTIME,
    get_runtime_field_name,
    get_settings_field_name,
    is_exempt_settings,
)

# =============================================================================
# Default registries
# =============================================================================
# INTERNAL_DEFAULTS: Hardcoded values not exposed in Settings
# POLICY_DEFAULTS: Defaults for plugin RetryPolicy dicts
from elspeth.contracts.config.defaults import (
    INTERNAL_DEFAULTS,
    POLICY_DEFAULTS,
)

# =============================================================================
# Runtime protocols
# =============================================================================
# Protocols that define what engine components expect from runtime config.
from elspeth.contracts.config.protocols import (
    RuntimeCheckpointProtocol,
    RuntimeConcurrencyProtocol,
    RuntimeRateLimitProtocol,
    RuntimeRetryProtocol,
    RuntimeTelemetryProtocol,
    ServiceRateLimitProtocol,
)

# =============================================================================
# Runtime configuration dataclasses
# =============================================================================
# Concrete implementations of Runtime*Protocol interfaces.
from elspeth.contracts.config.runtime import (
    ExporterConfig,
    RuntimeCheckpointConfig,
    RuntimeConcurrencyConfig,
    RuntimeRateLimitConfig,
    RuntimeRetryConfig,
    RuntimeServiceRateLimit,
    RuntimeTelemetryConfig,
)

# =============================================================================
# Settings classes are NOT re-exported here
# =============================================================================
# Settings classes (RetrySettings, ElspethSettings, etc.) live in core.config.
# They are NOT imported here to maintain contracts as a leaf module.
#
# Import Settings from elspeth.core.config:
#     from elspeth.core.config import RetrySettings, ElspethSettings
#
# =============================================================================

__all__ = [
    "EXEMPT_SETTINGS",
    "FIELD_MAPPINGS",
    "INTERNAL_DEFAULTS",
    "POLICY_DEFAULTS",
    "SETTINGS_TO_RUNTIME",
    "ExporterConfig",
    "RuntimeCheckpointConfig",
    "RuntimeCheckpointProtocol",
    "RuntimeConcurrencyConfig",
    "RuntimeConcurrencyProtocol",
    "RuntimeRateLimitConfig",
    "RuntimeRateLimitProtocol",
    "RuntimeRetryConfig",
    "RuntimeRetryProtocol",
    "RuntimeServiceRateLimit",
    "RuntimeTelemetryConfig",
    "RuntimeTelemetryProtocol",
    "ServiceRateLimitProtocol",
    "get_runtime_field_name",
    "get_settings_field_name",
    "is_exempt_settings",
]
