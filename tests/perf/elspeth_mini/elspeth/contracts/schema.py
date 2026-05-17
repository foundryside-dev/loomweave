"""Schema configuration types for config-driven plugin schemas.

This module provides the configuration types that allow users to specify
plugin schemas in their pipeline configuration files. Schemas can be:

1. Observed: Accept any fields, types inferred from data (logged for audit)
2. Fixed: Accept exactly the specified fields (no more, no less)
3. Flexible: Accept at least the specified fields (extras allowed)

Example YAML:
    plugins:
      csv_source:
        path: data.csv
        schema:
          mode: fixed
          fields:
            - "id: int"
            - "name: str"
            - "score: float?"  # Optional field

Note: Field specs must be quoted strings in YAML. Unquoted `- id: int` is parsed
as a dict `{id: int}` by YAML loaders, not as the string `"id: int"`. Both formats
are accepted, but quoted strings are recommended for clarity.
"""

from __future__ import annotations

import keyword
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Annotated, Any, Literal, cast

from pydantic import Field

# Supported field types for schema definitions
SUPPORTED_TYPES = frozenset({"str", "int", "float", "bool", "any"})

# Finite float: rejects NaN/Infinity per audit integrity requirements.
# NaN/Infinity cannot be represented in RFC 8785 canonical JSON and must be
# rejected at trust boundaries rather than silently corrupting the audit trail.
FiniteFloat = Annotated[float, Field(allow_inf_nan=False)]

# Canonical mapping from schema field type strings to Python/Pydantic types.
# Used by DAG graph validation (L1) and plugin schema factory (L3) to build
# dynamic Pydantic models from config-driven schema definitions.
FIELD_TYPE_MAP: dict[str, Any] = {
    "str": str,
    "int": int,
    "float": FiniteFloat,
    "bool": bool,
    "any": Any,
}

# Pattern: "field_name: type" or "field_name: type?"
# Field names must be valid Python identifiers (letters, digits, underscores)
# NOTE: Hyphens and dots are NOT supported in field names
#   - "user-id: int"  -> INVALID (use "user_id: int")
#   - "data.field: str" -> INVALID (use "data_field: str")
# This is intentional: field names map to Python attributes/dict keys
FIELD_PATTERN = re.compile(r"^(\w+):\s*(str|int|float|bool|any)(\?)?$")

# CLOSED LIST: only the text source has a fully deterministic observed-mode
# output shape today. Do not extend this set without design review; broader
# heuristic growth would rebuild runtime validation piecemeal in the composer.
_TEXT_HEURISTIC_PLUGINS: frozenset[str] = frozenset({"text"})


@dataclass(frozen=True, slots=True)
class FieldDefinition:
    """Definition of a single field in a schema.

    Attributes:
        name: Field name (must be valid Python identifier)
        field_type: One of: str, int, float, bool, any
        required: If False, field may be absent from the row
        nullable: If True, field value may be None when present. Used by
            coalesce merge logic to track whether a branch can produce None
            values for this field (affecting collision policy semantics).
    """

    name: str
    field_type: Literal["str", "int", "float", "bool", "any"]
    required: bool = True
    nullable: bool = False

    @classmethod
    def parse(cls, spec: str | Mapping[str, Any]) -> FieldDefinition:
        """Parse a field specification string or dict.

        Accepts two formats:
        - String: "name: str" or "score: float?"
        - Dict (round-trip format): {"name": "x", "type": "str", "required": True, "nullable": False}

        The dict format preserves all four states of (required, nullable) without loss,
        unlike the string format where `?` conflates nullable=True with required=False.

        Args:
            spec: Field spec string or dict from to_dict() round-trip

        Returns:
            FieldDefinition instance

        Raises:
            ValueError: If spec is malformed or type is unknown
        """
        # Handle dict format (round-trip from to_dict)
        if isinstance(spec, Mapping):
            if "name" not in spec or "type" not in spec:
                raise ValueError(f"Dict field spec must have 'name' and 'type' keys, got {sorted(spec.keys())}")
            name = spec["name"]
            field_type = spec["type"]
            if not isinstance(name, str) or not isinstance(field_type, str):
                raise ValueError("Dict field spec 'name' and 'type' must be strings")
            # Identifier validation: must match string-format rules for Python/Jinja/SQL safety
            if not name.isidentifier():
                # Provide helpful suggestion for common fixable cases
                if name and name[0].isdigit():
                    raise ValueError(
                        f"Invalid field name '{name}' in dict field spec. "
                        f"Field names must be valid Python identifiers "
                        f"(cannot start with a digit)."
                    )
                suggested = name.replace("-", "_").replace(".", "_")
                raise ValueError(
                    f"Invalid field name '{name}' in dict field spec. "
                    f"Field names must be valid Python identifiers "
                    f"(letters, digits, underscores only). "
                    f"Use '{suggested}' instead."
                )
            if field_type not in SUPPORTED_TYPES:
                raise ValueError(f"Unknown type '{field_type}' in dict field spec. Supported types: {', '.join(sorted(SUPPORTED_TYPES))}")
            # Required and nullable are mandatory for round-trip format.
            # Missing keys are Tier 1 data corruption — crash, don't default.
            if "required" not in spec:
                raise ValueError("Dict field spec must have 'required' key for round-trip format")
            if type(spec["required"]) is not bool:
                raise ValueError(f"Dict field spec 'required' must be bool, got {type(spec['required']).__name__}")
            if "nullable" not in spec:
                raise ValueError("Dict field spec must have 'nullable' key for round-trip format")
            nullable = spec["nullable"]
            if type(nullable) is not bool:
                raise ValueError(f"Dict field spec 'nullable' must be bool, got {type(nullable).__name__}")
            # Type narrowing for mypy
            dict_typed_field: Literal["str", "int", "float", "bool", "any"] = field_type  # type: ignore[assignment]
            return cls(
                name=name,
                field_type=dict_typed_field,
                required=spec["required"],
                nullable=nullable,
            )

        # Handle string format
        spec = spec.strip()
        match = FIELD_PATTERN.match(spec)

        if not match:
            # Check if it's a type issue vs format issue
            if ":" in spec:
                parts = spec.split(":", 1)
                name_part = parts[0].strip()
                type_part = parts[1].strip().rstrip("?")

                # Check for invalid type
                if type_part not in SUPPORTED_TYPES:
                    raise ValueError(
                        f"Unknown type '{type_part}' in field spec '{spec}'. Supported types: {', '.join(sorted(SUPPORTED_TYPES))}"
                    )

                # Check for invalid field name (hyphens, dots, etc.)
                if not name_part.isidentifier():
                    raise ValueError(
                        f"Invalid field name '{name_part}' in field spec '{spec}'. "
                        f"Field names must be valid Python identifiers "
                        f"(letters, digits, underscores only). "
                        f"Use '{name_part.replace('-', '_').replace('.', '_')}' instead."
                    )

            raise ValueError(f"Invalid field spec '{spec}'. Expected format: 'field_name: type' or 'field_name: type?'")

        name, field_type, optional_marker = match.groups()

        # Validate that name is a valid Python identifier
        # (regex allows numeric-prefixed names like "123field" which aren't valid)
        if not name.isidentifier():
            raise ValueError(
                f"Invalid field name '{name}' in field spec '{spec}'. "
                f"Field names must be valid Python identifiers "
                f"(cannot start with a digit)."
            )

        # FIELD_PATTERN regex guarantees field_type is one of the supported literals
        # This check is defense-in-depth in case regex is modified incorrectly
        if field_type not in SUPPORTED_TYPES:
            raise ValueError(f"Unsupported field type: {field_type}")
        # Cast needed because mypy can't infer type narrowing from set membership
        typed_field: Literal["str", "int", "float", "bool", "any"] = field_type  # type: ignore[assignment]  # narrowed by SUPPORTED_TYPES membership check above

        return cls(
            name=name,
            field_type=typed_field,
            required=optional_marker is None,
            nullable=optional_marker is not None,
        )

    def to_dict(self) -> dict[str, str | bool]:
        """Convert to dictionary for serialization.

        Includes all semantic fields for audit trail completeness (D4 fix).
        The nullable flag affects coalesce merge behavior and must be
        traceable in the audit record.
        """
        return {
            "name": self.name,
            "type": self.field_type,
            "required": self.required,
            "nullable": self.nullable,
        }


def _parse_field_names_list(value: Any, field_name: str) -> tuple[str, ...] | None:
    """Parse a list of field names for guaranteed_fields/required_fields.

    Args:
        value: Raw value from config (should be None or list of strings)
        field_name: Name of the config field for error messages

    Returns:
        Tuple of field names, or None if value is None

    Raises:
        ValueError: If value is not None, a list, or contains non-strings
    """
    if value is None:
        return None

    if not isinstance(value, (list, tuple)):
        raise ValueError(f"'{field_name}' must be a list of field names, got {type(value).__name__}")

    if len(value) == 0:
        return None  # Empty list is treated as unspecified

    result: list[str] = []
    for i, name in enumerate(value):
        if not isinstance(name, str):
            raise ValueError(f"'{field_name}[{i}]' must be a string, got {type(name).__name__}")
        name = name.strip()
        if not name:
            raise ValueError(f"'{field_name}[{i}]' cannot be empty or whitespace-only")
        if not name.isidentifier():
            raise ValueError(
                f"'{field_name}[{i}]' must be a valid Python identifier, got '{name}'. "
                f"Field names must contain only letters, digits, and underscores, "
                f"and cannot start with a digit."
            )
        result.append(name)

    # Check for duplicates
    if len(result) != len(set(result)):
        duplicates = sorted({n for n in result if result.count(n) > 1})
        raise ValueError(f"Duplicate field names in '{field_name}': {', '.join(duplicates)}")

    return tuple(result)


def _validate_contract_fields_subset(
    contract_fields: tuple[str, ...] | None,
    field_name: str,
    declared_names: frozenset[str],
) -> None:
    """Validate that contract fields are subsets of declared fields.

    For explicit schemas (mode=fixed/flexible), contract fields like guaranteed_fields,
    required_fields, and audit_fields must reference fields that actually exist in
    the declared schema. Typos would otherwise create false audit claims.

    Args:
        contract_fields: The contract field tuple to validate, or None
        field_name: Name of the field for error messages (e.g., "guaranteed_fields")
        declared_names: Set of declared field names to validate against

    Raises:
        ValueError: If any contract field is not in declared_names
    """
    if contract_fields is None:
        return

    undefined = set(contract_fields) - declared_names
    if undefined:
        raise ValueError(
            f"'{field_name}' contains fields not declared in schema: "
            f"{', '.join(sorted(undefined))}. "
            f"Declared fields are: {', '.join(sorted(declared_names))}."
        )


def _normalize_field_spec(spec: Any, *, index: int) -> str | Mapping[str, Any]:
    """Normalize a field spec for parsing.

    Accepts:
    - String: "field_name: type" or "field_name: type?"
    - Dict (from YAML): {"field_name": "type"} or {"field_name": "type?"}
    - Dict (round-trip): {"name": "x", "type": "str", "required": True, "nullable": False}

    For round-trip dict format (with "name" and "type" keys), returns the dict
    directly to preserve all fields including nullable. FieldDefinition.parse()
    handles both string and dict inputs.

    Args:
        spec: Field specification (string or dict)
        index: Index in fields list (for error messages)

    Returns:
        Normalized spec (string for YAML format, dict for round-trip format)

    Raises:
        ValueError: If spec format is invalid
    """
    if isinstance(spec, str):
        return spec

    if isinstance(spec, Mapping):
        # Handle to_dict() round-trip format: {"name": "x", "type": "str", "required": true, "nullable": false}
        # Return the dict directly so FieldDefinition.parse() can preserve all fields.
        if "name" in spec and "type" in spec:
            name = spec["name"]
            type_spec = spec["type"]
            if not isinstance(name, str):
                raise ValueError(f"Field spec at index {index}: 'name' must be a string, got {type(name).__name__}.")
            if not isinstance(type_spec, str):
                raise ValueError(f"Field spec at index {index}: 'type' must be a string, got {type(type_spec).__name__}.")
            if "required" not in spec:
                raise ValueError(
                    f"Field spec at index {index}: 'required' key is missing. "
                    f"Round-trip dict format requires 'name', 'type', and 'required' keys."
                )
            if type(spec["required"]) is not bool:
                raise ValueError(
                    f"Field spec at index {index}: 'required' must be a bool, "
                    f"got {type(spec['required']).__name__} ({spec['required']!r}). "
                    f"Use true/false (YAML) or True/False (Python), not strings."
                )
            # Require nullable for round-trip format (P2 soundness fix).
            # Missing nullable is Tier 1 data corruption — crash, don't default.
            if "nullable" not in spec:
                raise ValueError(
                    f"Field spec at index {index}: 'nullable' key is missing. "
                    f"Round-trip dict format requires 'name', 'type', 'required', and 'nullable' keys."
                )
            if type(spec["nullable"]) is not bool:
                raise ValueError(
                    f"Field spec at index {index}: 'nullable' must be a bool, "
                    f"got {type(spec['nullable']).__name__} ({spec['nullable']!r}). "
                    f"Use true/false (YAML) or True/False (Python), not strings."
                )
            # Return dict directly - FieldDefinition.parse() handles dict input
            return spec

        # YAML `- id: int` parses as {"id": "int"}
        if len(spec) != 1:
            raise ValueError(
                f"Field spec at index {index} is a dict with {len(spec)} keys. "
                f"Expected single-key dict like {{'field_name': 'type'}} or a string like 'field_name: type'."
            )
        name, type_spec = next(iter(spec.items()))
        if not isinstance(name, str) or not isinstance(type_spec, str):
            raise ValueError(
                f"Field spec at index {index}: dict keys and values must be strings, "
                f"got {{{type(name).__name__}: {type(type_spec).__name__}}}."
            )
        return f"{name}: {type_spec}"

    raise ValueError(
        f"Field spec at index {index} must be a string like 'field_name: type' "
        f"or a dict like {{'field_name': 'type'}}, got {type(spec).__name__}."
    )


@dataclass(frozen=True, slots=True)
class SchemaConfig:
    """Configuration for a plugin's data schema.

    A schema can be either observed (accept anything, infer types from data)
    or explicit (validate against specified fields).

    Schema Contracts (for DAG validation):
        - guaranteed_fields: Fields the producer GUARANTEES will exist AND are
          part of the stable API contract. Downstream can safely depend on these.
        - required_fields: Fields the consumer REQUIRES in input.
        - audit_fields: Fields that exist in output but are NOT part of the
          stability contract. These are for audit trail reconstruction and may
          change between versions. DAG validation does NOT enforce these.

    For explicit schemas (mode='fixed' or 'flexible'), the declared fields are
    implicitly guaranteed. Use guaranteed_fields/required_fields to express
    contracts for observed schemas that still have known field requirements.

    Example YAML for an observed schema with explicit contracts:
        schema:
          mode: observed
          guaranteed_fields: [customer_id, timestamp]  # Producer guarantees these

        schema:
          mode: observed
          required_fields: [customer_id, amount]  # Consumer requires these

        schema:
          mode: observed
          guaranteed_fields: [response, response_usage]  # Stable API
          audit_fields: [response_template_hash]  # May change between versions

    Attributes:
        mode: "fixed" (exact fields), "flexible" (at least these), or "observed" (infer from data)
        fields: List of FieldDefinitions, or None if observed
        is_observed: True if schema types are inferred from data
        guaranteed_fields: Field names the producer guarantees (for observed schemas)
        required_fields: Field names the consumer requires (for observed schemas)
        audit_fields: Field names that exist but are not part of stability contract
    """

    mode: Literal["fixed", "flexible", "observed"]
    fields: tuple[FieldDefinition, ...] | None
    guaranteed_fields: tuple[str, ...] | None = None
    required_fields: tuple[str, ...] | None = None
    audit_fields: tuple[str, ...] | None = None

    @property
    def is_observed(self) -> bool:
        """True if schema types are inferred from data (OBSERVED mode)."""
        return self.mode == "observed"

    @classmethod
    def from_dict(cls, config: Mapping[str, Any]) -> SchemaConfig:
        """Parse schema configuration from dict.

        Args:
            config: Dict with 'mode' key (required). For fixed/flexible modes,
                   'fields' is also required. Optional: 'guaranteed_fields',
                   'required_fields', 'audit_fields' for contracts.

        Returns:
            SchemaConfig instance

        Raises:
            ValueError: If config is invalid (including non-dict input)
        """
        if not isinstance(config, Mapping):
            raise ValueError(f"Schema config must be a Mapping, got {type(config).__name__}")

        # Parse contract fields (valid for all schema modes)
        guaranteed_fields = _parse_field_names_list(config.get("guaranteed_fields"), "guaranteed_fields")
        required_fields = _parse_field_names_list(config.get("required_fields"), "required_fields")
        audit_fields = _parse_field_names_list(config.get("audit_fields"), "audit_fields")

        # Mode is required for all schemas — crash on absence
        if "mode" not in config:
            raise ValueError("'mode' is required for all schema configs (e.g. 'fixed', 'flexible', 'observed')")
        mode = config["mode"]

        # Handle observed schema (mode: observed)
        if mode == "observed":
            # Observed schemas may have optional fields for contracts (guaranteed_fields)
            # but don't define explicit field types
            fields_value = config.get("fields")
            # Allow only None or [] for backwards compatibility; any other value
            # is an explicit schema declaration and must be rejected.
            if fields_value is not None and fields_value != [] and fields_value != ():
                raise ValueError(
                    "Observed schemas (mode: observed) cannot have explicit field definitions. "
                    "Use guaranteed_fields/required_fields for contracts instead."
                )
            return cls(
                mode="observed",
                fields=None,
                guaranteed_fields=guaranteed_fields,
                required_fields=required_fields,
                audit_fields=audit_fields,
            )

        # Explicit schema - requires mode and fields
        if mode is None:
            raise ValueError(
                "'mode' key is required in schema config. "
                "Use 'mode: observed' (infer types from data), "
                "'mode: fixed' (exactly these fields), or "
                "'mode: flexible' (at least these fields)."
            )

        if mode not in ("fixed", "flexible"):
            raise ValueError(f"Invalid schema mode '{mode}'. Expected 'fixed', 'flexible', or 'observed'.")

        # Explicit schemas require fields
        if "fields" not in config:
            raise ValueError(
                f"'fields' key is required for mode '{mode}'. "
                f"Provide explicit field definitions, or use 'mode: observed' to infer types from data."
            )

        fields_value = config["fields"]

        # Parse field list
        if not isinstance(fields_value, (list, tuple)):
            raise ValueError(f"Schema fields must be a list, got {type(fields_value).__name__}")

        if len(fields_value) == 0:
            raise ValueError("Schema must define at least one field for fixed/flexible modes. Use 'mode: observed' to accept any fields.")

        # Normalize field specs: accept both string and dict forms
        normalized_specs = []
        for i, f in enumerate(fields_value):
            spec = _normalize_field_spec(f, index=i)
            normalized_specs.append(spec)

        parsed_fields = tuple(FieldDefinition.parse(spec) for spec in normalized_specs)

        # Check for duplicate field names
        names = [f.name for f in parsed_fields]
        if len(names) != len(set(names)):
            duplicates = [n for n in names if names.count(n) > 1]
            raise ValueError(f"Duplicate field names in schema: {', '.join(sorted(set(duplicates)))}")

        # Validate contract fields are subsets of declared fields
        # For explicit schemas, typos in guaranteed/required/audit_fields would create
        # false audit claims - catch them at config load time
        declared_names = frozenset(names)
        _validate_contract_fields_subset(guaranteed_fields, "guaranteed_fields", declared_names)
        _validate_contract_fields_subset(required_fields, "required_fields", declared_names)
        _validate_contract_fields_subset(audit_fields, "audit_fields", declared_names)
        if guaranteed_fields is not None:
            required_declared_names = frozenset(f.name for f in parsed_fields if f.required)
            optional_guarantees = set(guaranteed_fields) - required_declared_names
            if optional_guarantees:
                raise ValueError(
                    "'guaranteed_fields' contains optional fields not guaranteed by schema: "
                    f"{', '.join(sorted(optional_guarantees))}. "
                    "Mark them required or remove them from guaranteed_fields."
                )

        return cls(
            mode=mode,
            fields=parsed_fields,
            guaranteed_fields=guaranteed_fields,
            required_fields=required_fields,
            audit_fields=audit_fields,
        )

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for audit logging.

        Contract fields (guaranteed_fields, required_fields) are included
        when present, omitted when None for cleaner audit output.
        """
        result: dict[str, Any]
        if self.is_observed:
            result = {"mode": "observed", "fields": None}
        else:
            result = {
                "mode": self.mode,
                "fields": [f.to_dict() for f in self.fields] if self.fields else [],
            }

        # Include contract fields only when specified (cleaner audit output)
        if self.guaranteed_fields is not None:
            result["guaranteed_fields"] = list(self.guaranteed_fields)
        if self.required_fields is not None:
            result["required_fields"] = list(self.required_fields)
        if self.audit_fields is not None:
            result["audit_fields"] = list(self.audit_fields)

        return result

    @property
    def declares_guaranteed_fields(self) -> bool:
        """Whether this schema explicitly declares what fields it guarantees.

        The None-vs-empty-tuple distinction on guaranteed_fields is semantic:
          None  = "no declaration" — the producer abstains from guarantee votes.
                  In coalesce intersection logic, this branch is skipped.
          ()    = "explicitly guarantees zero fields" — participates in
                  intersection and collapses the result to the empty set.

        Use this property instead of raw ``guaranteed_fields is not None``
        checks to centralise the abstain-vs-empty contract.

        See also:
            has_effective_guarantees: Use in coalesce merge loops to determine
                branch participation. Unlike this property, it also considers
                typed fields as a source of guarantees (not just explicit
                guaranteed_fields). For abstain-vs-empty semantic checks, use
                THIS property; for "does this branch contribute guarantees at
                all?", use has_effective_guarantees.
        """
        return self.guaranteed_fields is not None

    @property
    def has_effective_guarantees(self) -> bool:
        """Whether this schema has any source of field guarantees.

        Unlike declares_guaranteed_fields (which only checks explicit
        guaranteed_fields), this property also considers typed fields
        as a source of guarantees. A schema with mode="fixed", fields=(id, x)
        where both are required, but guaranteed_fields=None, still guarantees
        {id, x} via the type system.

        A schema with fields defined but all optional (no required fields) still
        participates in coalesce intersections — it has effective guarantees of
        the EMPTY SET. This is distinct from a schema with fields=None which
        abstains entirely from the intersection.

        Use this property in coalesce merge loops to determine if a branch
        should participate in the guarantee union/intersection. A branch with
        has_effective_guarantees=False is truly abstaining — no explicit
        declaration AND no typed fields at all.

        Returns:
          True if explicit guaranteed_fields is not None, OR
          True if schema has typed fields (regardless of required status).

        See also:
            declares_guaranteed_fields: Use for abstain-vs-empty semantic
                checks (None vs empty tuple). This property answers "did the
                schema make an explicit guarantee declaration?", while
                has_effective_guarantees answers "does the schema contribute
                to coalesce intersection/union at all?"
        """
        if self.guaranteed_fields is not None:
            return True
        # Fields defined = participates (even if all optional → contributes empty set)
        return self.fields is not None

    @property
    def participates_in_propagation(self) -> bool:
        """Whether this schema participates in ADR-007 pass-through propagation.

        True iff the schema has guarantees to contribute to the intersection
        that a downstream pass-through transform computes. False iff the
        schema abstains entirely.

        Canonical participation predicate per ADR-009 §Clause 1. Both the
        runtime DAG walker and the composer preview walker must consult this
        property rather than using the underlying ``declares_guaranteed_fields``
        or ``has_effective_guarantees`` directly, so future changes to
        participation semantics (e.g., Track 2's ``can_drop_rows`` declaration)
        land on this property alone.

        Uses ``has_effective_guarantees`` (explicit declarations OR typed
        fields). Fixed-mode schemas with typed required fields implicitly
        guarantee those fields even without an explicit ``guaranteed_fields``
        declaration; skipping them would under-propagate.
        """
        return self.has_effective_guarantees

    @property
    def allows_extra_fields(self) -> bool:
        """Whether extra fields beyond schema are allowed."""
        return self.is_observed or self.mode == "flexible"

    def get_effective_guaranteed_fields(self) -> frozenset[str]:
        """Get all fields this schema guarantees will exist.

        For explicit schemas (fixed/flexible mode), REQUIRED declared fields are
        implicitly guaranteed. Optional fields (marked with ?) are NOT guaranteed
        since producers are allowed to omit them. For observed schemas, only
        explicit guaranteed_fields are considered.

        Returns:
            Frozenset of field names that are guaranteed to exist.
        """
        # Start with explicit guaranteed_fields if any
        explicit = frozenset(self.guaranteed_fields) if self.guaranteed_fields else frozenset()

        # For explicit schemas, only REQUIRED fields are implicitly guaranteed
        # Optional fields (f.required == False) may be missing, so they're not guaranteed
        if self.fields is not None:
            declared_required = frozenset(f.name for f in self.fields if f.required)
            return explicit | declared_required

        return explicit

    def get_effective_required_fields(self) -> frozenset[str]:
        """Get all fields this schema requires in input.

        For explicit schemas (fixed/flexible mode), required declared fields
        (not optional) are implicitly required. For observed schemas, only
        explicit required_fields are considered.

        Returns:
            Frozenset of field names that are required.
        """
        # Start with explicit required_fields if any
        explicit = frozenset(self.required_fields) if self.required_fields else frozenset()

        # For explicit schemas, required declared fields are implicitly required
        if self.fields is not None:
            declared_required = frozenset(f.name for f in self.fields if f.required)
            return explicit | declared_required

        return explicit


def raw_options_have_schema(options: Mapping[str, Any]) -> bool:
    """Return whether raw plugin options expose schema config under either alias."""
    return "schema" in options or "schema_config" in options


def _get_raw_schema_value(options: Mapping[str, Any]) -> object | None:
    """Return raw schema config from plugin options, honoring both alias names."""
    if "schema" in options:
        return cast(object, options["schema"])
    if "schema_config" in options:
        return cast(object, options["schema_config"])
    return None


def parse_raw_schema_config(raw_schema: object, *, owner: str) -> SchemaConfig | None:
    """Parse raw schema config for contract validation.

    Raises ValueError for conditions that should surface as pipeline
    validation errors, not for programming errors at the call site.
    """
    if raw_schema is None:
        return None
    if not isinstance(raw_schema, Mapping):
        raise ValueError(f"{owner} schema config must be a mapping, got {type(raw_schema).__name__}")
    try:
        return SchemaConfig.from_dict(raw_schema)
    except ValueError as exc:
        raise ValueError(f"{owner} schema config: {exc}") from exc


def get_raw_schema_config(
    options: Mapping[str, Any],
    *,
    owner: str,
) -> SchemaConfig | None:
    """Parse raw schema config from plugin options, honoring both alias names."""
    return parse_raw_schema_config(_get_raw_schema_value(options), owner=owner)


def get_aggregation_contract_options(
    options: Mapping[str, Any],
    *,
    owner: str,
) -> tuple[Mapping[str, Any], str]:
    """Return the mapping that carries an aggregation's input contract.

    Aggregation nodes accept their input contract under either flat
    ``options`` or a nested ``options.options`` wrapper. This helper is the
    single source of truth for that alias resolution; callers in
    ``contracts/schema.py`` and ``web/composer/state.py`` rely on it.
    Raises ``ValueError`` when ``options["options"]`` exists but is not a
    ``Mapping`` — that is a misconfiguration, not a recoverable shape.
    """
    if "options" not in options:
        return options, owner

    nested_options = options["options"]
    if not isinstance(nested_options, Mapping):
        raise ValueError(f"{owner} aggregation wrapper options must be a mapping, got {type(nested_options).__name__}")
    return nested_options, f"{owner} options"


def _parse_raw_required_input_fields(
    value: object,
    *,
    owner: str,
    field_name: str,
) -> tuple[str, ...] | None:
    """Parse raw required_input_fields from node config.

    Raises ValueError for conditions that should surface as pipeline
    validation errors, not for programming errors at the call site.
    """
    if value is None:
        return None
    if isinstance(value, str):
        raise ValueError(f"{owner} {field_name} must be a list of field names, not a bare string")
    try:
        return _parse_field_names_list(value, field_name)
    except ValueError as exc:
        raise ValueError(f"{owner} {exc}") from exc


def get_raw_producer_guaranteed_fields(
    plugin_name: str | None,
    options: Mapping[str, Any],
    *,
    owner: str,
) -> frozenset[str]:
    """Return producer guarantees from raw plugin config.

    Raises ValueError for conditions that should surface as pipeline
    validation errors, not for programming errors at the call site.
    """
    schema_config = get_raw_schema_config(options, owner=owner)
    if schema_config is None:
        return frozenset()

    guaranteed = schema_config.get_effective_guaranteed_fields()
    if plugin_name in _TEXT_HEURISTIC_PLUGINS and schema_config.mode == "observed" and not schema_config.declares_guaranteed_fields:
        column = options.get("column")
        if isinstance(column, str) and column.isidentifier() and not keyword.iskeyword(column):
            return frozenset({column})
    return guaranteed


def get_raw_node_required_fields(
    options: Mapping[str, Any],
    *,
    owner: str,
    node_type: str | None = None,
) -> frozenset[str]:
    """Return explicit consumer requirements from raw node config.

    Raises ValueError for conditions that should surface as pipeline
    validation errors, not for programming errors at the call site.
    """
    if "required_input_fields" in options:
        required_input = _parse_raw_required_input_fields(
            options["required_input_fields"],
            owner=owner,
            field_name="required_input_fields",
        )
        if required_input is not None:
            return frozenset(required_input)

    contract_options = options
    contract_owner = owner
    if node_type == "aggregation":
        contract_options, contract_owner = get_aggregation_contract_options(options, owner=owner)
        if contract_options is not options and "required_input_fields" in contract_options:
            required_input = _parse_raw_required_input_fields(
                contract_options["required_input_fields"],
                owner=owner,
                field_name="options.required_input_fields",
            )
            if required_input is not None:
                return frozenset(required_input)

    schema_config = get_raw_schema_config(contract_options, owner=contract_owner)
    if schema_config is None or schema_config.required_fields is None:
        return frozenset()
    return frozenset(schema_config.required_fields)


def get_raw_sink_required_fields(
    options: Mapping[str, Any],
    *,
    owner: str,
) -> frozenset[str]:
    """Return sink requirements from raw sink config.

    Raises ValueError for conditions that should surface as pipeline
    validation errors, not for programming errors at the call site.
    """
    schema_config = get_raw_schema_config(options, owner=owner)
    if schema_config is None:
        return frozenset()
    return schema_config.get_effective_required_fields()
