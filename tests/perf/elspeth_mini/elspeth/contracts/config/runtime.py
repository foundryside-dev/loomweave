"""Runtime configuration dataclasses.

These dataclasses implement the Runtime*Protocol interfaces and provide
factory methods to construct from Settings objects or plugin policies.

Design Principles:
1. Frozen (immutable) - runtime config should never change mid-execution
2. Slots - memory efficient, prevents attribute typos
3. Protocol compliance - implements Runtime*Protocol for structural typing
4. Factory methods - from_settings(), from_policy(), default(), no_retry()

Field Origins:
- Settings fields: Come from user YAML configuration via Pydantic models
- Internal fields: Hardcoded implementation details, documented in INTERNAL_DEFAULTS

See alignment.py for complete field mapping documentation.
"""

import math
from collections.abc import Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import TYPE_CHECKING, Any

from elspeth.contracts.config.defaults import INTERNAL_DEFAULTS, POLICY_DEFAULTS
from elspeth.contracts.engine import RetryPolicy
from elspeth.contracts.enums import _IMPLEMENTED_BACKPRESSURE_MODES, BackpressureMode, TelemetryGranularity
from elspeth.contracts.freeze import freeze_fields, require_int

if TYPE_CHECKING:
    from elspeth.core.config import (
        CheckpointSettings,
        ConcurrencySettings,
        RateLimitSettings,
        RetrySettings,
        TelemetrySettings,
    )


def _merge_policy_with_defaults(policy: RetryPolicy) -> dict[str, Any]:
    """Merge policy with defaults, returning dict with numeric values.

    Policy values override defaults. The result has all POLICY_DEFAULTS keys
    with values from either policy (if present) or defaults.
    """
    return {**POLICY_DEFAULTS, **policy}


def _validate_int_field(field_name: str, value: Any) -> int:
    """Validate and convert a policy field to int.

    Args:
        field_name: Name of the field (for error messages)
        value: The value to validate and convert

    Returns:
        The value converted to int

    Raises:
        ValueError: If value is None, non-numeric, or cannot be converted
    """
    # Explicit None check - common misconfiguration
    if value is None:
        raise ValueError(f"Invalid retry policy: {field_name} must be numeric, got None")

    # Already int - pass through (but not bool, which is a subclass of int)
    if isinstance(value, int) and not isinstance(value, bool):
        return value

    # Float - convert to int (reject non-finite and non-integral values)
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError(f"Invalid retry policy: {field_name} must be finite, got {value}")
        if not value.is_integer():
            raise ValueError(f"Invalid retry policy: {field_name} must be an integer, got {value}")
        return int(value)

    # String - attempt numeric coercion
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError as exc:
            raise ValueError(f"Invalid retry policy: {field_name} must be numeric, got {value!r}") from exc

    # Non-numeric type (list, dict, bool, etc.)
    type_name = type(value).__name__
    raise ValueError(f"Invalid retry policy: {field_name} must be numeric, got {type_name}")


def _validate_float_field(field_name: str, value: Any) -> float:
    """Validate and convert a policy field to float.

    Args:
        field_name: Name of the field (for error messages)
        value: The value to validate and convert

    Returns:
        The value converted to float

    Raises:
        ValueError: If value is None, non-numeric, or cannot be converted
    """
    # Explicit None check - common misconfiguration
    if value is None:
        raise ValueError(f"Invalid retry policy: {field_name} must be numeric, got None")

    # Already float - pass through (reject NaN/Infinity for RFC 8785 compliance)
    if isinstance(value, float) and not isinstance(value, bool):
        if not math.isfinite(value):
            raise ValueError(f"Invalid retry policy: {field_name} must be finite, got {value}")
        return value

    # Int - convert to float (but not bool)
    if isinstance(value, int) and not isinstance(value, bool):
        return float(value)

    # String - attempt numeric coercion
    if isinstance(value, str):
        try:
            result = float(value)
        except ValueError as exc:
            raise ValueError(f"Invalid retry policy: {field_name} must be numeric, got {value!r}") from exc
        if not math.isfinite(result):
            raise ValueError(f"Invalid retry policy: {field_name} must be finite, got {value!r}")
        return result

    # Non-numeric type (list, dict, bool, etc.)
    type_name = type(value).__name__
    raise ValueError(f"Invalid retry policy: {field_name} must be numeric, got {type_name}")


@dataclass(frozen=True, slots=True)
class RuntimeRetryConfig:
    """Runtime configuration for retry behavior.

    Implements RuntimeRetryProtocol for structural typing verification.

    Field Origins:
        - max_attempts: RetrySettings.max_attempts (direct mapping)
        - base_delay: RetrySettings.initial_delay_seconds (renamed)
        - max_delay: RetrySettings.max_delay_seconds (renamed)
        - exponential_base: RetrySettings.exponential_base (direct mapping)
        - jitter: INTERNAL - hardcoded to 1.0 second (see INTERNAL_DEFAULTS["retry"]["jitter"])

    Why jitter is internal:
        Jitter adds randomness to backoff delays to prevent thundering herd
        problems when many clients retry simultaneously. The value (1.0 second)
        is a reasonable default that users shouldn't need to tune. Exposing it
        in Settings would add configuration surface without practical benefit.

    Note: max_attempts is the TOTAL number of tries, not the number of retries.
    So max_attempts=3 means: try, retry, retry (3 total).
    """

    max_attempts: int
    base_delay: float  # seconds
    max_delay: float  # seconds
    jitter: float  # seconds - INTERNAL, not from Settings
    exponential_base: float  # backoff multiplier

    def __post_init__(self) -> None:
        """Validate configuration values.

        Offensive checks: reject invalid ranges at construction time.
        Previously, from_policy() silently clamped values (e.g. -5 → 1),
        which masked configuration errors — the runtime behavior diverged
        from what the user configured, violating auditability.
        """
        require_int(self.max_attempts, "max_attempts", min_value=1)
        for field_name in ("base_delay", "max_delay", "jitter", "exponential_base"):
            value = getattr(self, field_name)
            if not math.isfinite(value):
                raise ValueError(f"{field_name} must be finite, got {value!r}")
        if self.base_delay < 0.01:
            raise ValueError(f"base_delay must be >= 0.01, got {self.base_delay}")
        if self.max_delay < 0.1:
            raise ValueError(f"max_delay must be >= 0.1, got {self.max_delay}")
        if self.jitter < 0.0:
            raise ValueError(f"jitter must be >= 0.0, got {self.jitter}")
        if self.exponential_base <= 1.0:
            raise ValueError(f"exponential_base must be > 1.0, got {self.exponential_base}")

    @classmethod
    def default(cls) -> "RuntimeRetryConfig":
        """Factory for default retry configuration.

        Uses POLICY_DEFAULTS values - the standard retry behavior.
        """
        return cls(
            max_attempts=int(POLICY_DEFAULTS["max_attempts"]),
            base_delay=float(POLICY_DEFAULTS["base_delay"]),
            max_delay=float(POLICY_DEFAULTS["max_delay"]),
            jitter=float(POLICY_DEFAULTS["jitter"]),
            exponential_base=float(POLICY_DEFAULTS["exponential_base"]),
        )

    @classmethod
    def no_retry(cls) -> "RuntimeRetryConfig":
        """Factory for no-retry configuration (single attempt).

        Useful for operations that should not be retried on failure.
        """
        return cls(
            max_attempts=1,
            base_delay=float(POLICY_DEFAULTS["base_delay"]),
            max_delay=float(POLICY_DEFAULTS["max_delay"]),
            jitter=float(POLICY_DEFAULTS["jitter"]),
            exponential_base=float(POLICY_DEFAULTS["exponential_base"]),
        )

    @classmethod
    def from_settings(cls, settings: "RetrySettings") -> "RuntimeRetryConfig":
        """Factory from RetrySettings config model.

        Field Mapping:
            settings.max_attempts -> max_attempts (direct)
            settings.initial_delay_seconds -> base_delay (renamed)
            settings.max_delay_seconds -> max_delay (renamed)
            settings.exponential_base -> exponential_base (direct)
            jitter <- INTERNAL_DEFAULTS["retry"]["jitter"] (hardcoded)

        Args:
            settings: Validated Pydantic settings model

        Returns:
            RuntimeRetryConfig with mapped values
        """
        return cls(
            max_attempts=settings.max_attempts,
            base_delay=settings.initial_delay_seconds,
            max_delay=settings.max_delay_seconds,
            jitter=float(INTERNAL_DEFAULTS["retry"]["jitter"]),
            exponential_base=settings.exponential_base,
        )

    @classmethod
    def from_policy(cls, policy: RetryPolicy | None) -> "RuntimeRetryConfig":
        """Factory from plugin policy dict.

        RetryPolicy is total=False (all fields optional), so plugins can specify
        partial overrides. Missing fields use POLICY_DEFAULTS.

        This is a trust boundary — plugin config (user YAML) may have invalid
        values. Non-numeric values (None, strings, lists, dicts) are rejected
        by the validate helpers. Out-of-range values (negative delays, zero
        attempts) are rejected by __post_init__. No silent clamping — if the
        user configured an invalid value, they get a clear error at startup.

        Args:
            policy: Optional RetryPolicy dict from plugin configuration.
                   If None, returns no_retry() configuration.

        Returns:
            RuntimeRetryConfig with policy values (or defaults for missing fields)

        Raises:
            ValueError: If any policy value is non-numeric (None, invalid string, etc.)
                       Error message includes field name and the invalid value.

        Note: We deliberately avoid .get() here. If a field exists in RuntimeRetryConfig
        but not in POLICY_DEFAULTS, the direct access below will crash. This is
        intentional - it catches the bug at development time, not production.
        """
        if policy is None:
            return cls.no_retry()

        # Merge explicit defaults with provided policy - policy values override
        full = _merge_policy_with_defaults(policy)

        # Validate and convert each field - raises ValueError with clear message
        # if any field has invalid type (None, non-numeric string, list, dict, etc.)
        max_attempts = _validate_int_field("max_attempts", full["max_attempts"])
        base_delay = _validate_float_field("base_delay", full["base_delay"])
        max_delay_val = _validate_float_field("max_delay", full["max_delay"])
        jitter = _validate_float_field("jitter", full["jitter"])
        exponential_base = _validate_float_field("exponential_base", full["exponential_base"])

        # Construct directly — __post_init__ validates ranges and raises
        # ValueError with clear messages for out-of-range values.
        # No silent clamping: if the user configured -5, they get an error,
        # not a silently different value.
        return cls(
            max_attempts=max_attempts,
            base_delay=base_delay,
            max_delay=max_delay_val,
            jitter=jitter,
            exponential_base=exponential_base,
        )


@dataclass(frozen=True, slots=True)
class RuntimeServiceRateLimit:
    """Runtime rate limit for a single service.

    Implements ServiceRateLimitProtocol. This is the Runtime-layer counterpart
    of ServiceRateLimit (Pydantic model in core/config.py), following the
    standard Settings→Runtime conversion pattern.
    """

    requests_per_minute: int

    def __post_init__(self) -> None:
        """Validate rate limit is positive."""
        require_int(self.requests_per_minute, "requests_per_minute", min_value=1)


@dataclass(frozen=True, slots=True)
class RuntimeRateLimitConfig:
    """Runtime configuration for rate limiting.

    Implements RuntimeRateLimitProtocol for structural typing verification.

    Field Origins (all from RateLimitSettings, direct mapping):
        - enabled: RateLimitSettings.enabled
        - default_requests_per_minute: RateLimitSettings.default_requests_per_minute
        - persistence_path: RateLimitSettings.persistence_path
        - services: RateLimitSettings.services (converted to RuntimeServiceRateLimit)

    Protocol Coverage:
        RuntimeRateLimitProtocol requires: enabled, default_requests_per_minute,
        persistence_path, and get_service_config(service_name).
        services remains a concrete RuntimeRateLimitConfig field used to
        implement get_service_config().

    Note: Unlike RetryConfig, there are no plugin-level rate limit overrides.
    Rate limiting is configured globally in Settings only.
    """

    enabled: bool
    default_requests_per_minute: int
    persistence_path: str | None
    services: Mapping[str, RuntimeServiceRateLimit]

    def __post_init__(self) -> None:
        """Validate rate limit config and freeze mutable services mapping."""
        require_int(self.default_requests_per_minute, "default_requests_per_minute", min_value=1)
        # Freeze services mapping to prevent external mutation
        object.__setattr__(self, "services", MappingProxyType(dict(self.services)))

    def get_service_config(self, service_name: str) -> RuntimeServiceRateLimit:
        """Get rate limit config for a service, with fallback to defaults.

        This mirrors RateLimitSettings.get_service_config() behavior, providing
        the same interface for RateLimitRegistry to use.

        Args:
            service_name: Name of the service to get config for

        Returns:
            RuntimeServiceRateLimit for the service (specific config if available,
            otherwise constructed from defaults)
        """
        if service_name in self.services:
            return self.services[service_name]

        return RuntimeServiceRateLimit(requests_per_minute=self.default_requests_per_minute)

    @classmethod
    def default(cls) -> "RuntimeRateLimitConfig":
        """Factory for default rate limit configuration.

        Returns disabled rate limiting with 60 requests/minute default.
        """
        return cls(
            enabled=False,
            default_requests_per_minute=60,
            persistence_path=None,
            services=MappingProxyType({}),
        )

    @classmethod
    def from_settings(cls, settings: "RateLimitSettings") -> "RuntimeRateLimitConfig":
        """Factory from RateLimitSettings config model.

        Field Mapping (all direct, no renames):
            settings.enabled -> enabled
            settings.default_requests_per_minute -> default_requests_per_minute
            settings.persistence_path -> persistence_path
            settings.services -> services (converted to RuntimeServiceRateLimit)

        Args:
            settings: Validated Pydantic settings model

        Returns:
            RuntimeRateLimitConfig with mapped values
        """
        return cls(
            enabled=settings.enabled,
            default_requests_per_minute=settings.default_requests_per_minute,
            persistence_path=settings.persistence_path,
            services=MappingProxyType(
                {k: RuntimeServiceRateLimit(requests_per_minute=v.requests_per_minute) for k, v in settings.services.items()}
            ),
        )


@dataclass(frozen=True, slots=True)
class RuntimeConcurrencyConfig:
    """Runtime configuration for concurrency/parallelism.

    Implements RuntimeConcurrencyProtocol for structural typing verification.

    Field Origins (all from ConcurrencySettings, direct mapping):
        - max_workers: ConcurrencySettings.max_workers

    This is the simplest runtime config - just one field controlling
    the maximum number of parallel workers for thread pool execution.

    Note: Unlike RetryConfig, there are no plugin-level concurrency overrides.
    Concurrency is configured globally in Settings only.
    """

    max_workers: int

    def __post_init__(self) -> None:
        """Validate configuration values."""
        require_int(self.max_workers, "max_workers", min_value=1)

    @classmethod
    def default(cls) -> "RuntimeConcurrencyConfig":
        """Factory for default concurrency configuration.

        Returns config with max_workers=4 (same as ConcurrencySettings default).
        """
        return cls(max_workers=4)

    @classmethod
    def from_settings(cls, settings: "ConcurrencySettings") -> "RuntimeConcurrencyConfig":
        """Factory from ConcurrencySettings config model.

        Field Mapping (direct, no renames):
            settings.max_workers -> max_workers

        Args:
            settings: Validated Pydantic settings model

        Returns:
            RuntimeConcurrencyConfig with mapped values
        """
        return cls(max_workers=settings.max_workers)


@dataclass(frozen=True, slots=True)
class RuntimeCheckpointConfig:
    """Runtime configuration for crash recovery checkpointing.

    Implements RuntimeCheckpointProtocol for structural typing verification.

    Field Origins (all from CheckpointSettings):
        - enabled: CheckpointSettings.enabled (direct mapping)
        - frequency: Computed from CheckpointSettings.frequency + checkpoint_interval
          - "every_row" -> 1
          - "every_n" -> checkpoint_interval value
          - "aggregation_only" -> 0
        - checkpoint_interval: CheckpointSettings.checkpoint_interval (direct mapping)
        - aggregation_boundaries: CheckpointSettings.aggregation_boundaries (direct mapping)

    Protocol Coverage:
        RuntimeCheckpointProtocol requires: enabled, frequency, aggregation_boundaries.
        The additional field (checkpoint_interval) is preserved for full Settings
        fidelity but not part of the protocol.

    Note on frequency type transformation:
        CheckpointSettings.frequency is a Literal["every_row", "every_n", "aggregation_only"].
        RuntimeCheckpointConfig.frequency is an int (checkpoint every N rows).
        This transformation happens in from_settings(), not through field name mapping.
    """

    enabled: bool
    frequency: int  # checkpoint every N rows (1=every_row, 0=aggregation_only)
    checkpoint_interval: int | None  # preserved from Settings for reference
    aggregation_boundaries: bool  # whether to checkpoint at aggregation flush

    def __post_init__(self) -> None:
        """Validate configuration values."""
        require_int(self.frequency, "frequency", min_value=0)
        require_int(self.checkpoint_interval, "checkpoint_interval", optional=True, min_value=0)

    @classmethod
    def default(cls) -> "RuntimeCheckpointConfig":
        """Factory for default checkpoint configuration.

        Returns config matching CheckpointSettings defaults:
        - enabled=True
        - frequency=1 (every_row)
        - checkpoint_interval=None
        - aggregation_boundaries=True
        """
        return cls(
            enabled=True,
            frequency=1,  # every_row
            checkpoint_interval=None,
            aggregation_boundaries=True,
        )

    @classmethod
    def from_settings(cls, settings: "CheckpointSettings") -> "RuntimeCheckpointConfig":
        """Factory from CheckpointSettings config model.

        Field Mapping:
            settings.enabled -> enabled (direct)
            settings.frequency + checkpoint_interval -> frequency (computed):
                - "every_row" -> 1
                - "every_n" -> checkpoint_interval value
                - "aggregation_only" -> 0
            settings.checkpoint_interval -> checkpoint_interval (direct)
            settings.aggregation_boundaries -> aggregation_boundaries (direct)

        Args:
            settings: Validated Pydantic settings model

        Returns:
            RuntimeCheckpointConfig with mapped values
        """
        # Map Literal frequency to int
        frequency: int
        if settings.frequency == "every_row":
            frequency = 1
        elif settings.frequency == "aggregation_only":
            frequency = 0
        else:
            # every_n uses checkpoint_interval
            # Settings validator ensures checkpoint_interval is set when frequency="every_n"
            if settings.checkpoint_interval is None:
                raise ValueError("checkpoint_interval required when checkpointing is enabled")
            frequency = settings.checkpoint_interval

        return cls(
            enabled=settings.enabled,
            frequency=frequency,
            checkpoint_interval=settings.checkpoint_interval,
            aggregation_boundaries=settings.aggregation_boundaries,
        )


@dataclass(frozen=True, slots=True)
class ExporterConfig:
    """Configuration for a single telemetry exporter.

    This is an immutable container for exporter settings. Each exporter
    has a name (which determines the exporter class) and options dict
    (passed to the exporter's constructor).

    Example YAML that produces ExporterConfig instances:
        telemetry:
          exporters:
            - name: console
              options:
                pretty: true
            - name: otlp
              options:
                endpoint: https://otel.example.com
    """

    name: str
    options: Mapping[str, Any]

    def __post_init__(self) -> None:
        """Validate exporter configuration."""
        if not self.name:
            raise ValueError("exporter name cannot be empty")
        freeze_fields(self, "options")


@dataclass(frozen=True, slots=True)
class RuntimeTelemetryConfig:
    """Runtime configuration for telemetry emission.

    Implements RuntimeTelemetryProtocol for structural typing verification.

    Field Origins (all from TelemetrySettings):
        - enabled: TelemetrySettings.enabled (direct mapping)
        - granularity: TelemetrySettings.granularity (parsed from str to enum)
        - backpressure_mode: TelemetrySettings.backpressure_mode (parsed from str to enum)
        - fail_on_total_exporter_failure: TelemetrySettings.fail_on_total_exporter_failure (direct)
        - max_consecutive_failures: TelemetrySettings.max_consecutive_failures (direct)
        - exporter_configs: TelemetrySettings.exporters (converted to tuple of ExporterConfig)

    Protocol Coverage:
        All fields are part of RuntimeTelemetryProtocol.

    Fail-Fast Behavior:
        The from_settings() factory validates that backpressure_mode is implemented.
        Unimplemented modes (like 'slow') cause NotImplementedError at config load time,
        not at runtime. This follows ELSPETH's principle of failing fast with clear errors.
    """

    enabled: bool
    granularity: TelemetryGranularity
    backpressure_mode: BackpressureMode
    fail_on_total_exporter_failure: bool
    max_consecutive_failures: int
    exporter_configs: tuple[ExporterConfig, ...]

    def __post_init__(self) -> None:
        """Validate telemetry config invariants."""
        require_int(self.max_consecutive_failures, "max_consecutive_failures", min_value=1)

    @classmethod
    def default(cls) -> "RuntimeTelemetryConfig":
        """Factory for default telemetry configuration.

        Returns config with telemetry disabled - telemetry is opt-in.
        No exporters are configured by default.
        """
        return cls(
            enabled=False,
            granularity=TelemetryGranularity.LIFECYCLE,
            backpressure_mode=BackpressureMode.BLOCK,
            fail_on_total_exporter_failure=True,
            max_consecutive_failures=10,
            exporter_configs=(),
        )

    @classmethod
    def from_settings(cls, settings: "TelemetrySettings") -> "RuntimeTelemetryConfig":
        """Factory from TelemetrySettings config model.

        Field Mapping:
            settings.enabled -> enabled (direct)
            settings.granularity -> granularity (parsed to enum)
            settings.backpressure_mode -> backpressure_mode (parsed to enum)
            settings.fail_on_total_exporter_failure -> fail_on_total_exporter_failure (direct)
            settings.exporters -> exporter_configs (converted to tuple of ExporterConfig)

        Args:
            settings: Validated Pydantic settings model

        Returns:
            RuntimeTelemetryConfig with mapped values

        Raises:
            NotImplementedError: If backpressure_mode is not yet implemented (e.g., 'slow')
            ValueError: If granularity or backpressure_mode is invalid
        """
        # Pydantic Literal["lifecycle", "rows", "full"] / Literal["block", "drop", "slow"]
        # validates settings.granularity / .backpressure_mode at construction, so the
        # enum constructors are guaranteed a valid lowercase string. No coercion here.
        granularity = TelemetryGranularity(settings.granularity)
        backpressure_mode = BackpressureMode(settings.backpressure_mode)

        # Fail fast on unimplemented backpressure modes
        if backpressure_mode not in _IMPLEMENTED_BACKPRESSURE_MODES:
            implemented = sorted(m.value for m in _IMPLEMENTED_BACKPRESSURE_MODES)
            raise NotImplementedError(f"backpressure_mode='{backpressure_mode.value}' is not yet implemented. Use one of: {implemented}")

        # Convert exporter list to tuple of ExporterConfig
        exporter_configs = tuple(ExporterConfig(name=exp.name, options=MappingProxyType(dict(exp.options))) for exp in settings.exporters)

        return cls(
            enabled=settings.enabled,
            granularity=granularity,
            backpressure_mode=backpressure_mode,
            fail_on_total_exporter_failure=settings.fail_on_total_exporter_failure,
            max_consecutive_failures=settings.max_consecutive_failures,
            exporter_configs=exporter_configs,
        )
