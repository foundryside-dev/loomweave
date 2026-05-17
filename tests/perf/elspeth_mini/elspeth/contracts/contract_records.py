"""Audit record types for schema contracts.

These types bridge SchemaContract (runtime) to Landscape storage (JSON serialization).
The pattern:
- Runtime: SchemaContract with Python types
- Storage: ContractAuditRecord with string type names
- Restore: to_schema_contract() converts back with integrity check
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal

from elspeth.contracts.errors import (
    AuditIntegrityError,
    ContractViolation,
    ExtraFieldViolation,
    MissingFieldViolation,
    TypeMismatchViolation,
)
from elspeth.contracts.type_normalization import CONTRACT_TYPE_MAP

if TYPE_CHECKING:
    from elspeth.contracts.schema_contract import FieldContract, SchemaContract

# Valid enum-like values for runtime validation (Literal types are static-only)
_VALID_MODES = frozenset({"FIXED", "FLEXIBLE", "OBSERVED"})
_VALID_SOURCES = frozenset({"declared", "inferred"})


@dataclass(frozen=True, slots=True)
class FieldAuditRecord:
    """Audit record for a single field in a schema contract.

    Immutable after creation - stores type information as strings
    for JSON serialization in the Landscape audit trail.

    Attributes:
        normalized_name: Dict key / Python identifier (e.g., "important_data")
        original_name: Display name from source (e.g., "'Important - Data !!'")
        python_type: Python type name as string (e.g., "int", "str")
        required: Whether field must be present in row
        source: "declared" (from config) or "inferred" (from first row observation)
        nullable: Whether None is a valid value (from T | None union types)
    """

    normalized_name: str
    original_name: str
    python_type: str
    required: bool
    source: Literal["declared", "inferred"]
    nullable: bool = False

    @classmethod
    def from_field_contract(cls, fc: FieldContract) -> FieldAuditRecord:
        """Create FieldAuditRecord from FieldContract.

        Converts the Python type to its string name for serialization.

        Args:
            fc: The FieldContract to convert

        Returns:
            FieldAuditRecord with type name as string
        """
        return cls(
            normalized_name=fc.normalized_name,
            original_name=fc.original_name,
            python_type=fc.python_type.__name__,
            required=fc.required,
            source=fc.source,
            nullable=fc.nullable,
        )

    def to_dict(self) -> dict[str, Any]:
        """Convert to JSON-serializable dictionary.

        Returns:
            Dict suitable for JSON serialization
        """
        return {
            "normalized_name": self.normalized_name,
            "original_name": self.original_name,
            "python_type": self.python_type,
            "required": self.required,
            "source": self.source,
            "nullable": self.nullable,
        }


@dataclass(frozen=True, slots=True)
class ContractAuditRecord:
    """Audit record for a schema contract.

    Stores schema contract information in a format suitable for
    JSON serialization in the Landscape audit trail.

    Attributes:
        mode: Schema enforcement mode (FIXED, FLEXIBLE, OBSERVED)
        locked: True after first row processed (types frozen)
        version_hash: Deterministic hash for integrity verification
        fields: Immutable tuple of FieldAuditRecord instances
    """

    mode: Literal["FIXED", "FLEXIBLE", "OBSERVED"]
    locked: bool
    version_hash: str
    fields: tuple[FieldAuditRecord, ...]

    @classmethod
    def from_contract(cls, contract: SchemaContract) -> ContractAuditRecord:
        """Create ContractAuditRecord from SchemaContract.

        Args:
            contract: The SchemaContract to convert

        Returns:
            ContractAuditRecord suitable for JSON serialization
        """
        sorted_fields = sorted(contract.fields, key=lambda fc: fc.normalized_name)
        return cls(
            mode=contract.mode,
            locked=contract.locked,
            version_hash=contract.version_hash(),
            fields=tuple(FieldAuditRecord.from_field_contract(fc) for fc in sorted_fields),
        )

    def to_json(self) -> str:
        """Serialize to canonical JSON.

        Uses canonical_json for deterministic serialization,
        ensuring identical contracts produce identical JSON.

        Returns:
            Canonical JSON string
        """
        from elspeth.contracts.hashing import canonical_json

        # Canonicalize field order so semantically equivalent contracts serialize
        # identically regardless of upstream insertion/merge ordering.
        sorted_fields = sorted(self.fields, key=lambda f: f.normalized_name)
        data = {
            "mode": self.mode,
            "locked": self.locked,
            "version_hash": self.version_hash,
            "fields": [f.to_dict() for f in sorted_fields],
        }
        return canonical_json(data)

    @classmethod
    def from_json(cls, json_str: str) -> ContractAuditRecord:
        """Restore ContractAuditRecord from JSON string.

        Args:
            json_str: JSON string from to_json()

        Returns:
            Restored ContractAuditRecord
        """
        data = json.loads(json_str)

        # Tier 1 structural validation: crash on malformed JSON shape.
        if not isinstance(data, dict):
            raise AuditIntegrityError(f"Contract audit record must be a JSON object, got {type(data).__name__}")
        if "fields" not in data or not isinstance(data["fields"], list):
            raise AuditIntegrityError("Contract audit record missing or malformed 'fields' — expected a list")
        for i, entry in enumerate(data["fields"]):
            if not isinstance(entry, dict):
                raise AuditIntegrityError(f"Contract audit record fields[{i}] must be a JSON object, got {type(entry).__name__}")

        # Tier 1 audit data: crash on invalid enum-like values.
        # Literal type hints are static-only; runtime validation is required
        # to reject corrupted/tampered records that would silently change
        # schema enforcement behavior.
        mode = data["mode"]
        if mode not in _VALID_MODES:
            raise AuditIntegrityError(f"Invalid contract mode '{mode}' in audit record. Valid modes: {', '.join(sorted(_VALID_MODES))}")

        # Tier 1: boolean fields must be exactly bool, not truthy ints/strings.
        # Non-boolean locked silently changes contract enforcement semantics.
        locked = data["locked"]
        if not isinstance(locked, bool):
            raise AuditIntegrityError(f"Contract 'locked' must be bool, got {type(locked).__name__}: {locked!r}")

        fields: list[FieldAuditRecord] = []
        for f in data["fields"]:
            source = f["source"]
            if source not in _VALID_SOURCES:
                raise AuditIntegrityError(
                    f"Invalid field source '{source}' for field "
                    f"'{f['normalized_name']}' in audit record. "
                    f"Valid sources: {', '.join(sorted(_VALID_SOURCES))}"
                )
            python_type = f["python_type"]
            if python_type not in CONTRACT_TYPE_MAP:
                raise AuditIntegrityError(
                    f"Invalid python_type '{python_type}' for field "
                    f"'{f['normalized_name']}' in audit record. "
                    f"Valid types: {', '.join(sorted(CONTRACT_TYPE_MAP.keys()))}"
                )
            # Tier 1: boolean fields must be exactly bool.
            required = f["required"]
            nullable = f["nullable"]
            if not isinstance(required, bool):
                raise AuditIntegrityError(
                    f"Field '{f['normalized_name']}' 'required' must be bool, got {type(required).__name__}: {required!r}"
                )
            if not isinstance(nullable, bool):
                raise AuditIntegrityError(
                    f"Field '{f['normalized_name']}' 'nullable' must be bool, got {type(nullable).__name__}: {nullable!r}"
                )
            fields.append(
                FieldAuditRecord(
                    normalized_name=f["normalized_name"],
                    original_name=f["original_name"],
                    python_type=python_type,
                    required=required,
                    source=source,
                    nullable=nullable,
                )
            )

        return cls(
            mode=mode,
            locked=locked,
            version_hash=data["version_hash"],
            fields=tuple(fields),
        )

    def to_schema_contract(self) -> SchemaContract:
        """Convert back to SchemaContract with integrity verification.

        Verifies the restored contract's hash matches the stored hash
        to ensure audit integrity (Tier 1 requirement).

        Returns:
            Restored SchemaContract

        Raises:
            AuditIntegrityError: If mode, source, or python_type is invalid,
                or if hash verification fails (integrity violation)
        """
        from elspeth.contracts.schema_contract import FieldContract, SchemaContract

        # Tier 1 audit data: validate enum-like values before constructing
        # a live SchemaContract. Invalid values would silently change
        # enforcement semantics (e.g., mode="BROKEN" skips extra-field rejection).
        if self.mode not in _VALID_MODES:
            raise AuditIntegrityError(
                f"Invalid contract mode '{self.mode}' in audit record. Valid modes: {', '.join(sorted(_VALID_MODES))}"
            )

        # Tier 1: validate boolean fields before constructing live contracts.
        if not isinstance(self.locked, bool):
            raise AuditIntegrityError(f"Contract 'locked' must be bool, got {type(self.locked).__name__}: {self.locked!r}")

        for f in self.fields:
            if f.source not in _VALID_SOURCES:
                raise AuditIntegrityError(
                    f"Invalid field source '{f.source}' for field "
                    f"'{f.normalized_name}' in audit record. "
                    f"Valid sources: {', '.join(sorted(_VALID_SOURCES))}"
                )
            if f.python_type not in CONTRACT_TYPE_MAP:
                raise AuditIntegrityError(
                    f"Invalid python_type '{f.python_type}' for field "
                    f"'{f.normalized_name}' in audit record. "
                    f"Valid types: {', '.join(sorted(CONTRACT_TYPE_MAP.keys()))}"
                )
            if not isinstance(f.required, bool):
                raise AuditIntegrityError(
                    f"Field '{f.normalized_name}' 'required' must be bool, got {type(f.required).__name__}: {f.required!r}"
                )
            if not isinstance(f.nullable, bool):
                raise AuditIntegrityError(
                    f"Field '{f.normalized_name}' 'nullable' must be bool, got {type(f.nullable).__name__}: {f.nullable!r}"
                )

        fields = tuple(
            FieldContract(
                normalized_name=f.normalized_name,
                original_name=f.original_name,
                python_type=CONTRACT_TYPE_MAP[f.python_type],
                required=f.required,
                source=f.source,
                nullable=f.nullable,
            )
            for f in self.fields
        )

        contract = SchemaContract(
            mode=self.mode,
            fields=fields,
            locked=self.locked,
        )

        # Verify integrity (Tier 1 audit requirement)
        actual_hash = contract.version_hash()
        if actual_hash != self.version_hash:
            raise AuditIntegrityError(
                f"Contract integrity violation: hash mismatch. "
                f"Expected {self.version_hash}, got {actual_hash}. "
                f"Audit record may be corrupted or from different version."
            )

        return contract


@dataclass(frozen=True, slots=True)
class ValidationErrorWithContract:
    """Audit record for a schema validation error.

    Captures contract violation details in a format suitable for
    the Landscape audit trail.

    Attributes:
        violation_type: Type of violation (type_mismatch, missing_field, extra_field)
        normalized_field_name: Internal field name used by code
        original_field_name: Original field name from external data
        expected_type: Expected type name (for type_mismatch), None otherwise
        actual_type: Actual type name (for type_mismatch), None otherwise
    """

    violation_type: Literal["type_mismatch", "missing_field", "extra_field"]
    normalized_field_name: str
    original_field_name: str
    expected_type: str | None
    actual_type: str | None

    @classmethod
    def from_violation(cls, violation: ContractViolation) -> ValidationErrorWithContract:
        """Create ValidationErrorWithContract from ContractViolation.

        Args:
            violation: The ContractViolation to convert

        Returns:
            ValidationErrorWithContract with violation details

        Raises:
            ValueError: If violation is of an unknown type
        """
        if isinstance(violation, TypeMismatchViolation):
            return cls(
                violation_type="type_mismatch",
                normalized_field_name=violation.normalized_name,
                original_field_name=violation.original_name,
                expected_type=violation.expected_type.__name__,
                actual_type=violation.actual_type.__name__,
            )
        elif isinstance(violation, MissingFieldViolation):
            return cls(
                violation_type="missing_field",
                normalized_field_name=violation.normalized_name,
                original_field_name=violation.original_name,
                expected_type=None,
                actual_type=None,
            )
        elif isinstance(violation, ExtraFieldViolation):
            return cls(
                violation_type="extra_field",
                normalized_field_name=violation.normalized_name,
                original_field_name=violation.original_name,
                expected_type=None,
                actual_type=None,
            )
        else:
            raise ValueError(f"Unknown violation type: {type(violation).__name__}")
