"""Schema contracts for preserving type and name information through the pipeline.

This module implements the Unified Schema Contracts design:
- FieldContract: Immutable field metadata (normalized name, original name, type)
- SchemaContract: Per-node schema with O(1) name resolution
- PipelineRow: Row wrapper enabling dual-name access
"""

from __future__ import annotations

import hashlib
import types
from collections.abc import Iterator, Mapping
from dataclasses import dataclass, field
from typing import Any, Literal

from elspeth.contracts.errors import (
    AuditIntegrityError,
    ContractMergeError,
    ContractViolation,
    ExtraFieldViolation,
    MissingFieldViolation,
    TypeMismatchViolation,
)
from elspeth.contracts.freeze import deep_freeze, deep_thaw
from elspeth.contracts.type_normalization import (
    ALLOWED_CONTRACT_TYPES,
    CONTRACT_TYPE_MAP,
    classify_runtime_type,
    normalize_type_for_contract,
)


@dataclass(frozen=True, slots=True)
class FieldContract:
    """A field in the schema contract.

    Immutable after creation - type locking means no mutation.
    Uses frozen dataclass pattern for checkpoint safety.

    Attributes:
        normalized_name: Dict key / Python identifier (e.g., "important_data")
        original_name: Display name from source (e.g., "'Important - Data !!'")
        python_type: Python primitive type (int, str, float, bool, datetime, type(None), object)
        required: Whether field must be present in row
        source: "declared" (from config) or "inferred" (from first row observation)
    """

    normalized_name: str
    original_name: str
    python_type: type
    required: bool
    source: Literal["declared", "inferred"]
    nullable: bool = False

    def __post_init__(self) -> None:
        """Validate python_type is checkpoint-serializable.

        Raises:
            TypeError: If python_type is not in ALLOWED_CONTRACT_TYPES
        """
        if self.python_type not in ALLOWED_CONTRACT_TYPES:
            raise TypeError(
                f"Invalid python_type '{self.python_type.__name__}' for FieldContract. "
                f"Valid types: {', '.join(sorted(t.__name__ for t in ALLOWED_CONTRACT_TYPES))}."
            )


@dataclass(frozen=True, slots=True)
class SchemaContract:
    """Immutable schema contract for a node.

    Uses frozen dataclass pattern - all "mutations" return new instances.

    Attributes:
        mode: Schema enforcement mode
            - FIXED: Exact fields only, extras rejected
            - FLEXIBLE: Declared minimum + inferred extras allowed
            - OBSERVED: All fields observed/inferred from data
        fields: Immutable tuple of FieldContract instances
        locked: True after first row processed (types frozen)
    """

    mode: Literal["FIXED", "FLEXIBLE", "OBSERVED"]
    fields: tuple[FieldContract, ...]
    locked: bool = False

    # Computed indices - populated in __post_init__
    _by_normalized: Mapping[str, FieldContract] = field(default_factory=dict, repr=False, compare=False, hash=False)
    _by_original: Mapping[str, str] = field(default_factory=dict, repr=False, compare=False, hash=False)

    def __post_init__(self) -> None:
        """Build O(1) lookup indices after initialization.

        Raises:
            ValueError: If duplicate normalized_name found in fields
        """
        # Defense-in-depth: validate uniqueness before building indices
        # (with_field() also checks, but direct construction must be safe)
        normalized_names = [fc.normalized_name for fc in self.fields]
        if len(normalized_names) != len(set(normalized_names)):
            duplicates = [n for n in normalized_names if normalized_names.count(n) > 1]
            raise ValueError(f"Duplicate normalized_name in fields: {set(duplicates)}")

        by_norm: dict[str, FieldContract] = {fc.normalized_name: fc for fc in self.fields}
        by_orig: dict[str, str] = {}
        for fc in self.fields:
            if fc.original_name in by_orig:
                raise ValueError(
                    f"Duplicate original_name in fields: '{fc.original_name}' maps to "
                    f"both '{by_orig[fc.original_name]}' and '{fc.normalized_name}'"
                )
            by_orig[fc.original_name] = fc.normalized_name
        object.__setattr__(self, "_by_normalized", types.MappingProxyType(by_norm))
        object.__setattr__(self, "_by_original", types.MappingProxyType(by_orig))

    def resolve_name(self, key: str) -> str:
        """Resolve original or normalized name to normalized name.

        O(1) lookup via precomputed indices.

        Args:
            key: Either an original_name or normalized_name

        Returns:
            The normalized_name for the field

        Raises:
            KeyError: If the key is not found in the schema
        """
        normalized = self.find_name(key)
        if normalized is None:
            raise KeyError(f"'{key}' not found in schema contract")
        return normalized

    def find_name(self, key: str) -> str | None:
        """Resolve original/normalized name when present (optional lookup).

        Args:
            key: Either an original_name or normalized_name

        Returns:
            The normalized_name if found, None otherwise
        """
        if key in self._by_normalized:
            return key  # Already normalized
        if key in self._by_original:
            return self._by_original[key]
        return None

    def get_field(self, normalized_name: str) -> FieldContract:
        """Get FieldContract by normalized name (strict lookup).

        Args:
            normalized_name: The normalized field name to look up

        Returns:
            The FieldContract for the requested field.

        Raises:
            KeyError: If the field is not found in the contract
        """
        try:
            return self._by_normalized[normalized_name]
        except KeyError as e:
            raise KeyError(f"'{normalized_name}' not found in schema contract") from e

    def find_field(self, normalized_name: str) -> FieldContract | None:
        """Find FieldContract by normalized name (optional lookup).

        Args:
            normalized_name: The normalized field name to look up

        Returns:
            The FieldContract if found, None otherwise
        """
        if normalized_name in self._by_normalized:
            return self._by_normalized[normalized_name]
        return None

    @property
    def required_field_names(self) -> frozenset[str]:
        """Normalized names of all fields with required=True."""
        return frozenset(fc.normalized_name for fc in self.fields if fc.required)

    def with_locked(self) -> SchemaContract:
        """Return new contract with locked=True.

        Once locked, types are frozen and cannot be changed.
        This is called after the first row is processed.

        Returns:
            New SchemaContract instance with locked=True
        """
        return SchemaContract(
            mode=self.mode,
            fields=self.fields,
            locked=True,
        )

    def with_field(
        self,
        normalized: str,
        original: str,
        value: Any,
    ) -> SchemaContract:
        """Return new contract with inferred field added.

        Called for OBSERVED/FLEXIBLE extras on first row.
        Returns new instance (frozen pattern).

        Args:
            normalized: Normalized field name (Python identifier)
            original: Original field name (from source)
            value: Sample value for type inference

        Returns:
            New SchemaContract with field added

        Raises:
            TypeError: If field already exists (duplicate prevention)
            ValueError: If value is NaN or Infinity
        """
        # Always check for duplicates - prevents broken O(1) lookup invariant
        # Per CLAUDE.md: Adding duplicate is a bug in caller code
        if normalized in self._by_normalized:
            raise TypeError(f"Field '{original}' ({normalized}) already exists in contract")

        new_field = FieldContract(
            normalized_name=normalized,
            original_name=original,
            python_type=normalize_type_for_contract(value),
            required=False,  # Inferred fields are never required
            source="inferred",
        )
        return SchemaContract(
            mode=self.mode,
            fields=(*self.fields, new_field),
            locked=self.locked,
        )

    def validate(self, row: dict[str, Any]) -> list[ContractViolation]:
        """Validate row data against contract.

        Checks:
        1. Required fields are present
        2. Field types match (with numpy/pandas normalization)
        3. FIXED mode: No extra fields allowed

        Note: Fields with python_type=object ('any' type) skip type validation
        since they accept any value type by design.

        Args:
            row: Row data with normalized field names as keys

        Returns:
            List of ContractViolation instances (empty if valid)
        """
        violations: list[ContractViolation] = []

        # Check declared fields
        for fc in self.fields:
            if fc.required and fc.normalized_name not in row:
                violations.append(
                    MissingFieldViolation(
                        normalized_name=fc.normalized_name,
                        original_name=fc.original_name,
                    )
                )
            elif fc.normalized_name in row:
                # Skip type check for 'any' fields (python_type is object)
                # 'any' type accepts any value - existence check above is sufficient
                if fc.python_type is object:
                    continue

                value = row[fc.normalized_name]

                # Allow None values for:
                # - Optional fields (required=False): field can be absent or None
                # - Nullable fields (nullable=True): field must be present but value can be None
                # This matches Pydantic semantics: score: float | None = field is required, but None is valid
                if value is None and (not fc.required or fc.nullable):
                    continue

                # Classify runtime type for comparison (non-throwing).
                # Uses classify_runtime_type instead of normalize_type_for_contract
                # so that unsupported types produce TypeMismatchViolation instead
                # of crashing the pipeline (Tier 3 data should be quarantined).
                actual_type = classify_runtime_type(value)
                # type(None) matches None values (for explicitly declared type(None) fields)
                if actual_type != fc.python_type:
                    violations.append(
                        TypeMismatchViolation(
                            normalized_name=fc.normalized_name,
                            original_name=fc.original_name,
                            expected_type=fc.python_type,
                            actual_type=actual_type,
                            actual_value=value,
                        )
                    )

        # FIXED mode: reject extra fields
        if self.mode == "FIXED":
            declared_names = {fc.normalized_name for fc in self.fields}
            for key in row:
                if key not in declared_names:
                    violations.append(
                        ExtraFieldViolation(
                            normalized_name=key,
                            original_name=key,  # We don't know original for extras
                        )
                    )

        return violations

    def version_hash(self) -> str:
        """Deterministic hash of contract for checkpoint references.

        Uses canonical JSON of ALL serialized state for reproducibility.
        The hash is truncated to 32 hex characters (128 bits).

        IMPORTANT: This hash MUST include ALL fields written by to_checkpoint_format().
        Per CLAUDE.md Tier 1: integrity checks must detect any mutation of serialized state.
        Missing fields from the hash would allow tampering without detection.

        Includes:
        - mode: Contract enforcement mode
        - locked: Whether types are frozen (tampering could allow inference on resume)
        - fields: All field definitions including 'source' (declared vs inferred)

        Returns:
            32-character hex hash string
        """
        from elspeth.contracts.hashing import canonical_json

        field_defs = [
            {
                "n": fc.normalized_name,
                "o": fc.original_name,
                "t": fc.python_type.__name__,
                "r": fc.required,
                "s": fc.source,  # Include source in hash - tampering = audit falsification
                "nullable": fc.nullable,
            }
            for fc in sorted(self.fields, key=lambda f: f.normalized_name)
        ]
        content = canonical_json(
            {
                "mode": self.mode,
                "locked": self.locked,  # Include locked in hash - tampering = security risk
                "fields": field_defs,
            }
        )
        return hashlib.sha256(content.encode()).hexdigest()[:32]

    def to_checkpoint_format(self) -> dict[str, Any]:
        """Full contract serialization for checkpoint storage.

        Includes version_hash for integrity verification on restore.

        Returns:
            Dict suitable for JSON serialization
        """
        return {
            "mode": self.mode,
            "locked": self.locked,
            "version_hash": self.version_hash(),
            "fields": [
                {
                    "normalized_name": fc.normalized_name,
                    "original_name": fc.original_name,
                    "python_type": fc.python_type.__name__,
                    "required": fc.required,
                    "source": fc.source,
                    "nullable": fc.nullable,
                }
                for fc in self.fields
            ],
        }

    @classmethod
    def from_checkpoint(cls, data: dict[str, Any]) -> SchemaContract:
        """Restore contract from checkpoint format.

        Validates hash integrity per Tier 1 audit requirements.

        Args:
            data: Dict from to_checkpoint_format()

        Returns:
            Restored SchemaContract

        Raises:
            KeyError: If checkpoint has unknown python_type
            ValueError: If restored hash doesn't match stored hash
        """
        # Tier 1 data: crash on corruption, but with informative messages.
        # to_checkpoint_format() writes "fields", "mode", "locked", "version_hash" —
        # if any are missing, the checkpoint is corrupted.
        try:
            fields = tuple(
                FieldContract(
                    normalized_name=f["normalized_name"],
                    original_name=f["original_name"],
                    python_type=CONTRACT_TYPE_MAP[f["python_type"]],
                    required=f["required"],
                    source=f["source"],
                    nullable=f["nullable"],
                )
                for f in data["fields"]
            )
        except KeyError as e:
            raise AuditIntegrityError(f"Corrupt SchemaContract checkpoint: missing key {e}. Top-level keys: {sorted(data.keys())}") from e

        try:
            contract = cls(
                mode=data["mode"],
                fields=fields,
                locked=data["locked"],
            )
        except KeyError as e:
            raise AuditIntegrityError(f"Corrupt SchemaContract checkpoint: missing key {e}. Top-level keys: {sorted(data.keys())}") from e

        # Verify integrity (Tier 1 audit requirement)
        # Per CLAUDE.md: "Bad data in the audit trail = crash immediately"
        # to_checkpoint_format() ALWAYS writes version_hash, so missing = corruption
        try:
            expected_hash = data["version_hash"]
        except KeyError as exc:
            raise AuditIntegrityError(
                f"Corrupt SchemaContract checkpoint: missing 'version_hash'. Top-level keys: {sorted(data.keys())}"
            ) from exc
        actual_hash = contract.version_hash()
        if actual_hash != expected_hash:
            raise AuditIntegrityError(
                f"Contract integrity violation: hash mismatch. "
                f"Expected {expected_hash}, got {actual_hash}. "
                f"Checkpoint may be corrupted or from different version."
            )

        return contract

    def merge(self, other: SchemaContract) -> SchemaContract:
        """Merge two contracts at a coalesce point.

        Rules:
        1. Mode: Most restrictive wins (FIXED > FLEXIBLE > OBSERVED)
        2. Fields present in both: Types must match; required only if both require (AND)
        3. Fields in only one: Included but marked non-required
        4. Locked: True if either is locked

        Args:
            other: Contract to merge with

        Returns:
            New merged SchemaContract

        Raises:
            ContractMergeError: If field types conflict
        """
        # Mode precedence: FIXED > FLEXIBLE > OBSERVED
        mode_order: dict[str, int] = {"FIXED": 0, "FLEXIBLE": 1, "OBSERVED": 2}
        merged_mode = min(
            self.mode,
            other.mode,
            key=lambda m: mode_order[m],
        )

        # Build merged field set
        merged_fields: dict[str, FieldContract] = {}

        all_names = {fc.normalized_name for fc in self.fields} | {fc.normalized_name for fc in other.fields}

        for name in sorted(all_names):
            in_self = name in self._by_normalized
            in_other = name in other._by_normalized

            if in_self and in_other:
                self_fc = self._by_normalized[name]
                other_fc = other._by_normalized[name]
                # Both have field - types must match
                if self_fc.python_type != other_fc.python_type:
                    raise ContractMergeError(
                        field=name,
                        type_a=self_fc.python_type.__name__,
                        type_b=other_fc.python_type.__name__,
                    )
                # Required only if BOTH branches require it (AND semantics).
                # NOTE: This is only correct for best_effort/quorum coalesces.
                # For require_all coalesces, OR semantics should apply (field is
                # required if required in ANY branch, since all branches arrive).
                # However, this fallback path is never reached in production —
                # the precomputed schema from merge_union_fields() is used instead.
                # See D2 investigation for details.
                # Use declared source if either is declared
                merged_fields[name] = FieldContract(
                    normalized_name=name,
                    original_name=self_fc.original_name,
                    python_type=self_fc.python_type,
                    required=self_fc.required and other_fc.required,
                    source="declared" if self_fc.source == "declared" or other_fc.source == "declared" else "inferred",
                    nullable=self_fc.nullable or other_fc.nullable,
                )
            elif in_self:
                # Only in self - include but mark non-required and nullable.
                # The source branch may not arrive (e.g., timeout under best_effort),
                # so the merged row would have None for this field. (D3 fix)
                fc = self._by_normalized[name]
                merged_fields[name] = FieldContract(
                    normalized_name=fc.normalized_name,
                    original_name=fc.original_name,
                    python_type=fc.python_type,
                    required=False,  # Can't require field from only one path
                    source=fc.source,
                    nullable=True,  # Branch may not arrive — field may be None
                )
            else:
                # Only in other (in_other must be True since name came from union).
                # Same reasoning: source branch may not arrive. (D3 fix)
                fc = other._by_normalized[name]
                merged_fields[name] = FieldContract(
                    normalized_name=fc.normalized_name,
                    original_name=fc.original_name,
                    python_type=fc.python_type,
                    required=False,  # Can't require field from only one path
                    source=fc.source,
                    nullable=True,  # Branch may not arrive — field may be None
                )

        return SchemaContract(
            mode=merged_mode,
            fields=tuple(merged_fields.values()),
            locked=self.locked or other.locked,
        )


class PipelineRow:
    """Row wrapper that enables dual-name access and type tracking.

    Wraps row data with a SchemaContract reference, enabling:
    - Access by normalized name: row["amount_usd"] or row.amount_usd
    - Access by original name: row["'Amount USD'"]
    - Type tracking via contract reference
    - Checkpoint serialization

    Uses __slots__ for memory efficiency (no __dict__ per instance).
    """

    __slots__ = ("_contract", "_data")

    def __init__(self, data: dict[str, Any], contract: SchemaContract) -> None:
        """Initialize PipelineRow.

        Args:
            data: Row data with normalized field names as keys
            contract: Schema contract for name resolution and validation

        Raises:
            TypeError: If data is not exactly a dict (Tier 1 enforcement).
                Non-dict inputs (e.g., list of tuples) would be silently coerced
                by dict(), masking data corruption on audit restore paths.
        """
        # Tier 1 enforcement: row data is system-owned after ingestion.
        # Coercing non-dict types (e.g., list of tuples via dict()) would mask
        # corruption in checkpoint restore and audit replay paths.
        if type(data) is not dict:
            raise TypeError(
                f"PipelineRow requires exactly dict, got {type(data).__name__}. "
                f"Non-dict input suggests data corruption on a Tier 1 restore path."
            )
        # Deep-freeze to prevent mutation of nested containers after audit recording.
        # Per CLAUDE.md Tier 1: audit data must not be modified.
        self._data = deep_freeze(data)
        self._contract = contract

    def __setitem__(self, key: str, value: Any) -> None:
        """Raise TypeError - PipelineRow is immutable.

        Args:
            key: Field name
            value: Value to set

        Raises:
            TypeError: Always - PipelineRow is immutable for audit integrity
        """
        raise TypeError("PipelineRow is immutable - cannot modify audit data")

    def __getitem__(self, key: str) -> Any:
        """Access field by original OR normalized name.

        Args:
            key: Field name (either original or normalized)

        Returns:
            Field value

        Raises:
            KeyError: If field not found in contract or data
        """
        try:
            normalized = self._contract.resolve_name(key)
            return self._data[normalized]
        except KeyError:
            # For FLEXIBLE mode: allow access to extra fields in data not in contract
            # For FIXED mode: resolve_name raises KeyError which we re-raise
            if self._contract.mode in ("FLEXIBLE", "OBSERVED") and key in self._data:
                return self._data[key]
            raise

    def __getattr__(self, key: str) -> Any:
        """Dot notation access: row.field_name.

        Args:
            key: Attribute name (must be normalized field name)

        Returns:
            Field value

        Raises:
            AttributeError: If field not found
        """
        # Prevent infinite recursion for private attributes
        if key.startswith("_"):
            raise AttributeError(key)
        try:
            return self[key]
        except KeyError as e:
            raise AttributeError(key) from e

    def __contains__(self, key: str) -> bool:
        """Support 'if field in row' checks.

        Checks if field is accessible via the contract AND exists in actual data.
        This enables guard patterns like 'if field in row: use(row[field])'
        to work correctly for optional fields that may be absent from data.

        Args:
            key: Field name (original or normalized)

        Returns:
            True if field is accessible via __getitem__ AND exists in actual row data.
            For FLEXIBLE/OBSERVED contracts, includes extra fields present in data.
            For FIXED contracts, only includes fields in the contract.

        Note:
            This must align with __getitem__ semantics: if 'key in row' returns True,
            then row[key] must NOT raise KeyError. This consistency is critical for
            gate expressions that check membership before accessing fields.
        """
        try:
            resolved = self._contract.resolve_name(key)
            return resolved in self._data
        except KeyError:
            # For FLEXIBLE/OBSERVED mode: allow access to extra fields in data not in contract
            # This mirrors __getitem__ fallback logic for extra-field access
            # For FIXED mode: resolve_name raises KeyError which we caught, return False
            return self._contract.mode in ("FLEXIBLE", "OBSERVED") and key in self._data

    def to_dict(self) -> dict[str, Any]:
        """Export raw data (normalized keys) for serialization.

        Returns:
            Mutable deep copy of internal data dict
        """
        thawed = deep_thaw(self._data)
        if not isinstance(thawed, dict):
            raise TypeError(f"deep_thaw(PipelineRow._data) must return dict, got {type(thawed).__name__}")
        return thawed

    def get(self, key: str, default: Any = None) -> Any:
        """Get field value with optional default (Jinja2 compatibility).

        Supports dual-name resolution like __getitem__.

        Args:
            key: Field name (original or normalized)
            default: Value to return if field not found

        Returns:
            Field value or default
        """
        try:
            return self[key]
        except KeyError:
            return default

    def keys(self) -> list[str]:
        """Return normalized field names (Jinja2 compatibility).

        Returns:
            List of normalized field names from the underlying data
        """
        return list(self._data.keys())

    def __iter__(self) -> Iterator[str]:
        """Iterate over normalized field names (Jinja2 compatibility).

        Yields:
            Normalized field names
        """
        return iter(self._data)

    @property
    def contract(self) -> SchemaContract:
        """Access the schema contract (for introspection/debugging).

        Returns:
            The SchemaContract associated with this row
        """
        return self._contract

    def __copy__(self) -> PipelineRow:
        """Support shallow copy - creates new PipelineRow with same data.

        SchemaContract is immutable (frozen=True), so sharing reference is safe.
        Data is deep-frozen, so thaw→re-freeze produces an independent copy.

        Returns:
            New PipelineRow with same contract and independent data copy
        """
        return PipelineRow(self.to_dict(), self._contract)

    def __deepcopy__(self, memo: dict[int, Any]) -> PipelineRow:
        """Support deep copy - creates new PipelineRow with deep copied data.

        This enables fork_token and expand_token to use deepcopy() on PipelineRow
        without hitting MappingProxyType pickle issues.

        SchemaContract is immutable (frozen=True), so sharing reference is safe.
        to_dict() deep-thaws, then PipelineRow.__init__ deep-freezes again.

        Args:
            memo: Memoization dict for deepcopy

        Returns:
            New PipelineRow with same contract and deep copied data
        """
        return PipelineRow(self.to_dict(), self._contract)

    def to_checkpoint_format(self) -> dict[str, Any]:
        """Serialize for checkpoint storage.

        Returns dict with data and contract reference (not full contract).
        Contract is stored once per node, not per row.

        Returns:
            Checkpoint-serializable dict
        """
        thawed = deep_thaw(self._data)
        if not isinstance(thawed, dict):
            raise TypeError(f"deep_thaw(PipelineRow._data) must return dict, got {type(thawed).__name__}")
        return {
            "data": thawed,
            "contract_version": self._contract.version_hash(),
        }

    @classmethod
    def from_checkpoint(
        cls,
        checkpoint_data: dict[str, Any],
        contract_registry: dict[str, SchemaContract],
    ) -> PipelineRow:
        """Restore from checkpoint.

        Args:
            checkpoint_data: Output from to_checkpoint_format()
            contract_registry: Node contracts indexed by version_hash

        Returns:
            Restored PipelineRow

        Raises:
            AuditIntegrityError: If checkpoint is missing required keys or contract
                version not in registry (Tier 1 data corruption).
        """
        try:
            version = checkpoint_data["contract_version"]
        except KeyError as exc:
            raise AuditIntegrityError(
                f"Corrupt PipelineRow checkpoint: missing 'contract_version'. Available keys: {sorted(checkpoint_data.keys())}"
            ) from exc

        try:
            contract = contract_registry[version]
        except KeyError as exc:
            raise AuditIntegrityError(
                f"Contract version '{version}' not in registry. Available versions: {sorted(contract_registry.keys())}"
            ) from exc

        try:
            data = checkpoint_data["data"]
        except KeyError as exc:
            raise AuditIntegrityError(
                f"Corrupt PipelineRow checkpoint: missing 'data'. Available keys: {sorted(checkpoint_data.keys())}"
            ) from exc

        return cls(data=data, contract=contract)
