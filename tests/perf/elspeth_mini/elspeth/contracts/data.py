"""Pydantic-based schema system for plugins.

This is the ONLY location for PluginSchema - import from elspeth.contracts.

Every plugin declares input and output schemas using Pydantic models.
This enables:
- Runtime validation of row data
- Pipeline validation at config time (Phase 3)
- Documentation generation
- Landscape context recording

TRUST BOUNDARY: PluginSchema validates "Their Data" (user rows from sources,
transform outputs) - NOT "Our Data" (audit trail). Therefore it uses permissive
settings (extra="ignore", strict=False, frozen=False) per the Data Manifesto.
"""

from dataclasses import dataclass
from types import UnionType
from typing import Annotated, Any, TypeVar, Union, get_args, get_origin

from pydantic import BaseModel, ConfigDict, ValidationError
from pydantic.fields import FieldInfo

T = TypeVar("T", bound="PluginSchema")


class PluginSchema(BaseModel):
    """Base class for plugin input/output schemas.

    Plugins define schemas by subclassing:

        class MyInputSchema(PluginSchema):
            temperature: float
            humidity: float

    Features:
    - Extra fields ignored (rows may have more fields than schema requires)
    - Coercive type validation (int->float allowed, strict=False)
    - Easy conversion to/from row dicts
    """

    model_config = ConfigDict(
        extra="ignore",  # Rows may have extra fields
        strict=False,  # Allow coercion (e.g., int -> float)
        frozen=False,  # Allow modification
    )

    def to_row(self) -> dict[str, Any]:
        """Convert schema instance to row dict."""
        return self.model_dump()

    @classmethod
    def from_row(cls: type[T], row: dict[str, Any]) -> T:
        """Create schema instance from row dict.

        Extra fields in row are ignored.
        """
        return cls.model_validate(row)


class SchemaValidationError:
    """A validation error for a specific field."""

    def __init__(self, field: str, message: str, value: Any = None) -> None:
        self.field = field
        self.message = message
        self.value = value

    def __str__(self) -> str:
        return f"{self.field}: {self.message}"

    def __repr__(self) -> str:
        return f"SchemaValidationError({self.field!r}, {self.message!r})"


def validate_row(
    row: dict[str, Any],
    schema: type[PluginSchema],
) -> list[SchemaValidationError]:
    """Validate a row against a schema.

    Args:
        row: Row data to validate
        schema: PluginSchema subclass

    Returns:
        List of validation errors (empty if valid)
    """
    try:
        schema.model_validate(row)
        return []
    except ValidationError as e:
        errors = []
        for error in e.errors():
            field = ".".join(str(loc) for loc in error["loc"])
            errors.append(
                SchemaValidationError(
                    field=field,
                    message=error["msg"],
                    value=error["input"],
                )
            )
        return errors


@dataclass(frozen=True, slots=True)
class CompatibilityResult:
    """Result of schema compatibility check.

    Frozen: compatibility results are immutable evidence of a schema check.
    List fields use tuples for deep immutability.
    """

    compatible: bool
    missing_fields: tuple[str, ...] = ()
    type_mismatches: tuple[tuple[str, str, str], ...] = ()
    extra_fields: tuple[str, ...] = ()
    constraint_mismatches: tuple[tuple[str, str], ...] = ()

    @property
    def error_message(self) -> str | None:
        """Human-readable error message if incompatible."""
        if self.compatible:
            return None

        parts = []
        if self.missing_fields:
            parts.append(f"Missing fields: {', '.join(self.missing_fields)}")
        if self.type_mismatches:
            mismatches = [f"{name} (expected {expected}, got {actual})" for name, expected, actual in self.type_mismatches]
            parts.append(f"Type mismatches: {', '.join(mismatches)}")
        if self.constraint_mismatches:
            constraints = [f"{name}: {reason}" for name, reason in self.constraint_mismatches]
            parts.append(f"Constraint mismatches: {', '.join(constraints)}")
        if self.extra_fields:
            parts.append(f"Extra fields forbidden by consumer: {', '.join(self.extra_fields)}")

        return "; ".join(parts)


def _get_allow_inf_nan(field: FieldInfo) -> bool | None:
    """Extract the allow_inf_nan constraint from a FieldInfo's metadata.

    Returns False if the field explicitly disallows inf/nan, True if it
    explicitly allows them, or None if the field has no such constraint
    (Pydantic default: inf/nan allowed).

    Pydantic v2 stores Field(allow_inf_nan=False) in FieldInfo.metadata
    as a _PydanticGeneralMetadata or AllowInfNan object with an
    ``allow_inf_nan`` attribute in its __dict__.
    """
    for item in field.metadata:
        try:
            item_vars = vars(item)
        except TypeError:
            # Metadata items like plain strings don't have __dict__
            continue
        if "allow_inf_nan" in item_vars:
            value: bool = item_vars["allow_inf_nan"]
            return value
    return None


def _check_field_constraints(
    field_name: str,
    producer_field: FieldInfo,
    consumer_field: FieldInfo,
) -> str | None:
    """Check if consumer field constraints are satisfied by producer field.

    Returns a human-readable reason string if incompatible, None if compatible.

    Constraint direction: if the consumer requires a constraint (e.g.,
    allow_inf_nan=False) that the producer does not guarantee, the producer
    might emit values the consumer will reject at runtime.

    Only checks constraints where mismatch causes runtime validation failure.
    """
    # allow_inf_nan: consumer requires finite floats but producer doesn't guarantee them.
    # None means "no constraint" (Pydantic default), which allows inf/nan.
    consumer_allows = _get_allow_inf_nan(consumer_field)
    producer_allows = _get_allow_inf_nan(producer_field)
    if consumer_allows is False and producer_allows is not False:
        # Only flag if producer's base type can actually produce non-finite values.
        # int values are always finite; only float/Decimal can be NaN/Infinity.
        producer_base = _unwrap_annotated(producer_field.annotation)
        if producer_base is not int:
            return "consumer requires finite floats (allow_inf_nan=False) but producer does not guarantee it"

    return None


def check_compatibility(
    producer_schema: type[PluginSchema],
    consumer_schema: type[PluginSchema],
) -> CompatibilityResult:
    """Check if producer output is compatible with consumer input.

    Uses Pydantic model_fields metadata for accurate compatibility checking.
    This handles optional fields, unions, constrained types, and defaults.

    Compatibility means:
    - All REQUIRED fields in consumer are provided by producer
    - Fields with defaults in consumer are optional
    - Field types are compatible (exact match or coercible when consumer allows)
    - Consumer constraints are satisfied by producer (e.g., allow_inf_nan=False)
    - If consumer has extra="forbid", producer must not have extra fields
    - If consumer has strict=True, no type coercion is allowed (int->float rejected)

    Args:
        producer_schema: Output schema of upstream plugin
        consumer_schema: Input schema of downstream plugin

    Returns:
        CompatibilityResult indicating compatibility and any issues
    """
    # Use Pydantic v2 model_fields for accurate field introspection
    producer_fields = producer_schema.model_fields
    consumer_fields = consumer_schema.model_fields

    # Check if consumer schema is strict (no type coercion allowed)
    # NOTE: We control all schemas via PluginSchema base class which sets model_config["strict"].
    # Direct access is correct per Tier 1 trust model - missing key would be our bug.
    consumer_strict = consumer_schema.model_config["strict"]

    missing: list[str] = []
    mismatches: list[tuple[str, str, str]] = []
    constraint_mismatches: list[tuple[str, str]] = []

    for field_name, consumer_field in consumer_fields.items():
        # Check if field is required (no default value)
        is_required = consumer_field.is_required()

        if field_name not in producer_fields:
            # Missing field - only a problem if required
            if is_required:
                missing.append(field_name)
        else:
            producer_field = producer_fields[field_name]
            if not _types_compatible(
                producer_field.annotation,
                consumer_field.annotation,
                consumer_strict=consumer_strict,
            ):
                mismatches.append(
                    (
                        field_name,
                        _type_name(consumer_field.annotation),
                        _type_name(producer_field.annotation),
                    )
                )
            else:
                # Types are compatible — check if consumer has stricter constraints
                # than producer guarantees.  E.g., consumer requires finite floats
                # (allow_inf_nan=False) but producer emits unconstrained floats.
                constraint_reason = _check_field_constraints(
                    field_name,
                    producer_field,
                    consumer_field,
                )
                if constraint_reason is not None:
                    constraint_mismatches.append((field_name, constraint_reason))

    # Check for extra fields when consumer forbids them
    extra: list[str] = []
    # NOTE: We control all schemas via PluginSchema base class which sets model_config["extra"].
    # Direct access is correct per Tier 1 trust model - missing key would be our bug.
    consumer_forbids_extras = consumer_schema.model_config["extra"] == "forbid"
    if consumer_forbids_extras:
        producer_field_names = set(producer_fields.keys())
        consumer_field_names = set(consumer_fields.keys())
        extra = sorted(producer_field_names - consumer_field_names)

    compatible = len(missing) == 0 and len(mismatches) == 0 and len(constraint_mismatches) == 0 and len(extra) == 0

    return CompatibilityResult(
        compatible=compatible,
        missing_fields=tuple(missing),
        type_mismatches=tuple(mismatches),
        extra_fields=tuple(extra),
        constraint_mismatches=tuple(constraint_mismatches),
    )


def _type_name(t: Any) -> str:
    """Get readable name for a type annotation.

    For generic types (Optional, Union, list[T], etc.), returns the full
    representation rather than just the origin type name.
    """
    # For generic types, use str() to get full representation
    origin = get_origin(t)
    if origin is not None:
        # Generic type - str() gives readable form like "list[str]" or "int | None"
        return str(t)
    # For simple types, use __name__ directly.
    # Falls back to str(t) for typing module specials (e.g. typing.Any)
    # that lack __name__.
    try:
        return str(t.__name__)
    except AttributeError:
        return str(t)


def _is_union_type(t: Any) -> bool:
    """Check if type is a Union (typing.Union or types.UnionType)."""
    origin = get_origin(t)
    return origin is Union or isinstance(t, UnionType)


def _unwrap_annotated(annotation: Any) -> Any:
    """Unwrap typing.Annotated recursively to its underlying type.

    Annotated[T, ...] wraps a type with metadata (e.g., Pydantic constraints).
    For BASE TYPE comparison we strip metadata here.  Semantic constraints
    (e.g., allow_inf_nan=False) are checked separately by _check_field_constraints
    using the FieldInfo objects, which Pydantic populates from the Annotated metadata.

    Examples:
        Annotated[float, FieldInfo(allow_inf_nan=False)] -> float
        Annotated[Annotated[int, ...], ...] -> int  (nested unwrap)
        float -> float  (no-op for non-Annotated)
    """
    current = annotation
    while get_origin(current) is Annotated:
        args = get_args(current)
        if not args:
            return current
        current = args[0]
    return current


def _types_compatible(
    actual: Any,
    expected: Any,
    *,
    consumer_strict: bool = False,
) -> bool:
    """Check if actual type is compatible with expected type.

    Handles:
    - Exact matches
    - Any type (accepts everything)
    - Numeric compatibility (int -> float) - ONLY when consumer_strict=False
    - Optional[X] on consumer side (producer can send X or X | None)
    - Union types with coercion (int compatible with float | None when not strict)
    - Annotated[T, ...] unwrapping (metadata stripped before comparison)

    Args:
        actual: The producer's output type annotation
        expected: The consumer's input type annotation
        consumer_strict: If True, no type coercion allowed (int->float rejected).
                        Respects Data Manifesto: transforms/sinks must NOT coerce.
    """
    # Unwrap Annotated metadata before any comparisons.
    # Config-generated schemas may wrap types (e.g., FiniteFloat -> Annotated[float, ...]).
    # We need the semantic base type for compatibility, not the constraint metadata.
    actual = _unwrap_annotated(actual)
    expected = _unwrap_annotated(expected)

    # Exact match
    if actual == expected:
        return True

    # Any accepts everything
    if expected is Any:
        return True

    # Numeric compatibility (int -> float is OK) - but ONLY when consumer allows coercion
    # Per Data Manifesto: transforms/sinks with strict=True must NOT coerce
    if expected is float and actual is int:
        return not consumer_strict

    # Handle Optional/Union types (both typing.Union and types.UnionType)
    if _is_union_type(expected):
        expected_args = get_args(expected)
        # Check if actual type matches any of the union members (with coercion rules)
        if any(_types_compatible(actual, expected_member, consumer_strict=consumer_strict) for expected_member in expected_args):
            return True
        # Check if actual is a Union where all members are compatible
        if _is_union_type(actual):
            actual_args = get_args(actual)
            return all(any(_types_compatible(a, e, consumer_strict=consumer_strict) for e in expected_args) for a in actual_args)

    return False
