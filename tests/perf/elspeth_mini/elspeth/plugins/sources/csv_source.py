"""CSV source plugin for ELSPETH.

Loads rows from CSV files using csv.reader for proper multiline quoted field support.

IMPORTANT: Sources use allow_coercion=True to normalize external data.
This is the ONLY place in the pipeline where coercion is allowed.
"""

import codecs
import csv
from collections.abc import Iterator, Mapping
from typing import Any

from pydantic import Field, ValidationError, field_validator

from elspeth.contracts import PluginSchema, SourceRow
from elspeth.contracts.contexts import SourceContext
from elspeth.contracts.contract_builder import ContractBuilder
from elspeth.contracts.schema_contract_factory import create_contract_from_config
from elspeth.plugins.infrastructure.base import BaseSource
from elspeth.plugins.infrastructure.config_base import TabularSourceDataConfig
from elspeth.plugins.infrastructure.schema_factory import create_schema_from_config
from elspeth.plugins.sources.field_normalization import FieldResolution, resolve_field_names


class CSVSourceConfig(TabularSourceDataConfig):
    """Configuration for CSV source plugin.

    Inherits from TabularSourceDataConfig, which provides:
    - schema and on_validation_failure (from SourceDataConfig)
    - columns, field_mapping (field normalization is mandatory)
    """

    delimiter: str = Field(default=",", description="Single-character delimiter used to split CSV fields.")
    encoding: str = Field(default="utf-8", description="Text encoding used to decode the CSV file.")
    skip_rows: int = Field(default=0, ge=0, description="Number of leading physical rows to skip before reading headers or data.")

    @field_validator("delimiter")
    @classmethod
    def _validate_delimiter(cls, v: str) -> str:
        if len(v) != 1:
            raise ValueError(f"delimiter must be a single character, got {v!r}")
        return v

    @field_validator("encoding")
    @classmethod
    def _validate_encoding(cls, v: str) -> str:
        try:
            codecs.lookup(v)
        except LookupError as exc:
            raise ValueError(f"unknown encoding: {v!r}") from exc
        return v


class CSVSource(BaseSource):
    """Load rows from a CSV file.

    Config options:
        path: Path to CSV file (required)
        schema: Schema configuration (required, via SourceDataConfig)
        delimiter: Field delimiter (default: ",")
        encoding: File encoding (default: "utf-8")
        skip_rows: Number of header rows to skip (default: 0)

    Field normalization:
        Headers are always normalized to valid Python identifiers at the
        source boundary. This is mandatory and not configurable.
        field_mapping: Override specific normalized names (optional)
        columns: Explicit column names for headerless files (optional)

    The schema can be:
        - Observed: {"mode": "observed"} - accept any fields
        - Fixed: {"mode": "fixed", "fields": ["id: int", "name: str"]}
        - Flexible: {"mode": "flexible", "fields": ["id: int"]} - at least these fields
    """

    name = "csv"
    plugin_version = "1.0.0"
    source_file_hash: str | None = "sha256:9f3618e6d858c0a9"
    config_model = CSVSourceConfig
    # Override parent type - SourceDataConfig requires this to be set
    _on_validation_failure: str

    def __init__(self, config: dict[str, Any]) -> None:
        super().__init__(config)
        cfg = CSVSourceConfig.from_dict(config, plugin_name=self.name)

        self._path = cfg.resolved_path()
        self._delimiter = cfg.delimiter
        self._encoding = cfg.encoding
        self._skip_rows = cfg.skip_rows

        # Store normalization config for use in load()
        self._columns = cfg.columns
        self._field_mapping = cfg.field_mapping

        # Field resolution computed at load() time - includes version for audit
        self._field_resolution: FieldResolution | None = None

        # Store schema config for audit trail (required by DataPluginConfig)
        self._schema_config = cfg.schema_config
        self._initialize_declared_guaranteed_fields(self._schema_config)

        # Store quarantine routing destination
        self._on_validation_failure = cfg.on_validation_failure
        # on_success is injected by the instantiation bridge (cli_helpers.py)

        # CRITICAL: allow_coercion=True for sources (external data boundary)
        # Sources are the ONLY place where type coercion is allowed
        self._schema_class: type[PluginSchema] = create_schema_from_config(
            self._schema_config,
            "CSVRowSchema",
            allow_coercion=True,
        )

        # Set output_schema for protocol compliance
        self.output_schema = self._schema_class

        # Create initial schema contract (may be updated after first row)
        # Contract creation deferred until load() when field_resolution is known
        self._contract_builder: ContractBuilder | None = None

    def load(self, ctx: SourceContext) -> Iterator[SourceRow]:
        """Load rows from CSV file with mandatory field normalization.

        Uses csv.reader directly on file handle to properly support
        multiline quoted fields (e.g., "field with\nembedded newline").

        Field resolution modes:
        - Headers from file: Always normalized to valid Python identifiers
        - columns=[...]: Headerless file, use explicit column names

        Each row is validated against the configured schema:
        - Valid rows are yielded as SourceRow.valid()
        - Invalid rows are yielded as SourceRow.quarantined()

        Yields:
            SourceRow for each row (valid or quarantined).

        Raises:
            FileNotFoundError: If CSV file does not exist.
            ValueError: If field collision detected after normalization,
                       or column count mismatch in headerless mode.
        """
        if not self._path.exists():
            raise FileNotFoundError(f"CSV file not found: {self._path}")

        # CRITICAL: newline='' required for proper embedded newline handling
        # See: https://docs.python.org/3/library/csv.html
        try:
            f = open(self._path, encoding=self._encoding, newline="")  # noqa: SIM115
        except UnicodeDecodeError as e:
            # Some encodings (e.g., utf-16) can fail at open() on BOM/header bytes.
            # This is Tier 3 (external data) — quarantine, don't crash.
            raw_row = {"file_path": str(self._path), "__encoding__": self._encoding}
            error_msg = f"CSV file cannot be decoded with encoding '{self._encoding}': {e}"
            ctx.record_validation_error(
                row=raw_row,
                error=error_msg,
                schema_mode="parse",
                destination=self._on_validation_failure,
            )
            if self._on_validation_failure != "discard":
                yield SourceRow.quarantined(
                    row=raw_row,
                    error=error_msg,
                    destination=self._on_validation_failure,
                )
            return

        try:
            yield from self._load_from_file(f, ctx)
        except UnicodeDecodeError as e:
            # Decode failure while reading rows — Tier 3 boundary.
            # Record parse-level error and stop (remaining rows may be corrupt).
            # File-level decode errors occur before CSV parsing — no meaningful
            # line number exists. Don't fabricate "unknown" or use dead getattr
            # (file objects don't have lineno; csv.reader does, but it's not
            # in scope here).
            raw_row = {
                "file_path": str(self._path),
                "__encoding__": self._encoding,
            }
            error_msg = f"CSV decode error (encoding '{self._encoding}'): {e}"
            ctx.record_validation_error(
                row=raw_row,
                error=error_msg,
                schema_mode="parse",
                destination=self._on_validation_failure,
            )
            if self._on_validation_failure != "discard":
                yield SourceRow.quarantined(
                    row=raw_row,
                    error=error_msg,
                    destination=self._on_validation_failure,
                )
        finally:
            f.close()

    def _load_from_file(self, f: Any, ctx: SourceContext) -> Iterator[SourceRow]:
        """Load rows from an open CSV file handle.

        Extracted from load() to allow UnicodeDecodeError handling at the
        file-reading boundary (Tier 3 external data). All csv.Error and
        StopIteration handling remains here.

        The caller (load()) is responsible for closing f.

        Args:
            f: Open file handle for the CSV file
            ctx: Plugin context for recording errors

        Yields:
            SourceRow for each row (valid or quarantined)
        """
        # Create csv.reader on file handle for multiline field support.
        # strict=True is required so malformed quoted input fails at the
        # source boundary instead of being silently merged into later rows.
        reader = csv.reader(f, delimiter=self._delimiter, strict=True)

        # Skip CSV records as configured (not raw lines), preserving multiline alignment.
        # skip_rows targets non-CSV metadata preamble (comments, version headers, etc.)
        # that may contain unmatched quotes or other RFC 4180 violations.
        #
        # CRITICAL: csv.Error during skip means the parser consumed an unknown amount
        # of data (e.g., an unmatched quote swallowed subsequent lines). We record the
        # error and stop processing to avoid silent data loss from corrupted parser state.
        for skip_idx in range(self._skip_rows):
            try:
                if next(reader, None) is None:
                    # Fewer rows than skip_rows — file exhausted during skip.
                    # Record that we ran out of data so the audit trail shows
                    # skip_rows consumed everything (no silent empty result).
                    skip_count = skip_idx  # rows successfully skipped before exhaustion
                    error_msg = (
                        f"CSV file exhausted during skip_rows; "
                        f"skip_rows={self._skip_rows} requested but file "
                        f"only had {skip_count} row(s) to skip "
                        f"(no header or data rows remain)"
                    )
                    raw_row = {
                        "file_path": str(self._path),
                        "skip_rows": self._skip_rows,
                        "rows_skipped": skip_count,
                    }
                    ctx.record_validation_error(
                        row=raw_row,
                        error=error_msg,
                        schema_mode="parse",
                        destination=self._on_validation_failure,
                    )
                    return
            except csv.Error as e:
                # Parser error during skip — the csv reader state may be corrupted
                # (e.g., unmatched quote consumed subsequent lines). Record the error
                # and stop processing to prevent silent data loss.
                physical_line = reader.line_num if reader.line_num > 0 else skip_idx + 1
                raw_row = {
                    "file_path": str(self._path),
                    "__line_number__": physical_line,
                    "__raw_line__": f"(csv.Error during skip_rows at row {skip_idx + 1})",
                }
                error_msg = f"CSV parse error during skip_rows at row {skip_idx + 1} (line {physical_line}): {e}"
                ctx.record_validation_error(
                    row=raw_row,
                    error=error_msg,
                    schema_mode="parse",
                    destination=self._on_validation_failure,
                )
                if self._on_validation_failure != "discard":
                    yield SourceRow.quarantined(
                        row=raw_row,
                        error=error_msg,
                        destination=self._on_validation_failure,
                    )
                return  # Don't continue with corrupted parser state

        def next_nonblank_record() -> list[str]:
            """Return the next nonblank CSV record.

            csv.reader yields [] for blank physical lines. We apply the same
            skip rule before header discovery and during data iteration so a
            leading blank line cannot become a zero-column header.
            """
            while True:
                values = next(reader)
                if values:
                    return values

        # Determine headers based on config
        if self._columns is not None:
            # Headerless mode - use explicit columns
            raw_headers = None
        else:
            # Read header row from file
            try:
                raw_headers = next_nonblank_record()
            except StopIteration:
                # File exhausted after skip_rows — no header row remains.
                # Record so the audit trail shows skip_rows consumed all content.
                if self._skip_rows > 0:
                    error_msg = (
                        f"CSV file has no header row after skipping {self._skip_rows} row(s); skip_rows may exceed available content"
                    )
                    ctx.record_validation_error(
                        row={
                            "file_path": str(self._path),
                            "skip_rows": self._skip_rows,
                        },
                        error=error_msg,
                        schema_mode="parse",
                        destination=self._on_validation_failure,
                    )
                return
            except csv.Error as e:
                # Header parse failure at source boundary (Tier 3): record and quarantine/discard
                physical_line = reader.line_num if reader.line_num > 0 else self._skip_rows + 1
                raw_row = {
                    "file_path": str(self._path),
                    "__line_number__": physical_line,
                    "__raw_line__": "(unparseable CSV header)",
                }
                error_msg = f"CSV parse error at line {physical_line}: {e}"

                ctx.record_validation_error(
                    row=raw_row,
                    error=error_msg,
                    schema_mode="parse",
                    destination=self._on_validation_failure,
                )

                if self._on_validation_failure != "discard":
                    yield SourceRow.quarantined(
                        row=raw_row,
                        error=error_msg,
                        destination=self._on_validation_failure,
                    )
                return
        # Resolve field names (normalization + mapping)
        # This may raise ValueError on collision
        self._field_resolution = resolve_field_names(
            raw_headers=raw_headers,
            field_mapping=self._field_mapping,
            columns=self._columns,
        )
        headers = self._field_resolution.final_headers
        expected_count = len(headers)

        # Create initial contract with field resolution
        initial_contract = create_contract_from_config(
            self._schema_config,
            field_resolution=self._field_resolution.resolution_mapping,
        )
        self._contract_builder = ContractBuilder(initial_contract)

        # Track whether first valid row has been processed (for type inference)
        first_valid_row_processed = False

        # Process data rows with manual iteration to catch csv.Error per row
        row_num = 0  # Logical row number (data rows only)
        while True:
            try:
                # Try to read next row - csv.Error raised here for malformed rows
                values = next_nonblank_record()
            except StopIteration:
                break  # End of file
            except csv.Error as e:
                # CSV parsing error (bad quoting, unmatched quotes, etc.)
                # CRITICAL: csv.Error can leave the parser in a corrupted state where
                # subsequent next() calls skip, merge, or misattribute rows.  The
                # skip_rows path already stops on csv.Error for this reason (see above).
                # We must do the same here — record the failure and stop processing.
                row_num += 1
                physical_line = reader.line_num
                raw_row = {
                    "__raw_line__": "(unparseable due to csv.Error)",
                    "__line_number__": physical_line,
                    "__row_number__": row_num,
                }
                error_msg = (
                    f"CSV parse error at line {physical_line}: {e}. "
                    f"Stopping file processing — csv.Error can corrupt parser state, "
                    f"making subsequent rows untrustworthy."
                )

                ctx.record_validation_error(
                    row=raw_row,
                    error=error_msg,
                    schema_mode="parse",
                    destination=self._on_validation_failure,
                )

                if self._on_validation_failure != "discard":
                    yield SourceRow.quarantined(
                        row=raw_row,
                        error=error_msg,
                        destination=self._on_validation_failure,
                    )
                return  # Don't continue with corrupted parser state

            row_num += 1
            # reader.line_num tracks physical file line position (including multiline fields)
            physical_line = reader.line_num

            # Column count validation - quarantine malformed rows in both header and headerless modes
            # Per Three-Tier Trust Model: source data is Tier 3 (zero trust), quarantine bad rows
            if len(values) != expected_count:
                raw_row = {
                    "__raw_line__": self._delimiter.join(values),
                    "__line_number__": physical_line,
                    "__row_number__": row_num,
                }
                error_msg = f"CSV parse error at line {physical_line}: expected {expected_count} fields, got {len(values)}"

                ctx.record_validation_error(
                    row=raw_row,
                    error=error_msg,
                    schema_mode="parse",
                    destination=self._on_validation_failure,
                )

                if self._on_validation_failure != "discard":
                    yield SourceRow.quarantined(
                        row=raw_row,
                        error=error_msg,
                        destination=self._on_validation_failure,
                    )
                continue

            # Build row dict
            row = dict(zip(headers, values, strict=False))

            # Validate row against schema
            try:
                validated = self._schema_class.model_validate(row)
                validated_row = validated.to_row()

                # Process first valid row for type inference
                if not first_valid_row_processed:
                    self._contract_builder.process_first_row(
                        validated_row,
                        self._field_resolution.resolution_mapping,
                    )
                    self.set_schema_contract(self._contract_builder.contract)
                    first_valid_row_processed = True

                # Validate against locked contract to catch type drift on
                # inferred fields. Pydantic extra="allow" accepts any type
                # for extras — the contract enforces inferred types here.
                contract = self.require_schema_contract()
                if contract.locked:
                    violations = contract.validate(validated_row)
                    if violations:
                        error_msg = "; ".join(str(v) for v in violations)
                        ctx.record_validation_error(
                            row=validated_row,
                            error=error_msg,
                            schema_mode=self._schema_config.mode,
                            destination=self._on_validation_failure,
                        )
                        if self._on_validation_failure != "discard":
                            yield SourceRow.quarantined(
                                row=validated_row,
                                error=error_msg,
                                destination=self._on_validation_failure,
                            )
                        continue

                yield SourceRow.valid(
                    validated_row,
                    contract=contract,
                )
            except ValidationError as e:
                ctx.record_validation_error(
                    row=row,
                    error=str(e),
                    schema_mode=self._schema_config.mode,
                    destination=self._on_validation_failure,
                )

                if self._on_validation_failure != "discard":
                    yield SourceRow.quarantined(
                        row=row,
                        error=str(e),
                        destination=self._on_validation_failure,
                    )

        # CRITICAL: Handle empty source case (all rows quarantined or no rows)
        # If no valid rows were processed, the contract is still unlocked.
        # Lock it now so downstream consumers have a consistent contract state.
        if not first_valid_row_processed and self._contract_builder is not None:
            self.set_schema_contract(self._contract_builder.contract.with_locked())

    def close(self) -> None:
        """Release resources (no-op for CSV source)."""
        pass

    def get_field_resolution(self) -> tuple[Mapping[str, str], str | None] | None:
        """Return field resolution mapping for audit trail.

        Returns the mapping from original CSV headers to final field names,
        computed during load() via mandatory field normalization or field_mapping.

        Returns:
            Tuple of (resolution_mapping, normalization_version) if field resolution
            was computed, or None if load() hasn't been called yet or no normalization
            was needed.
        """
        if self._field_resolution is None:
            return None

        return (
            self._field_resolution.resolution_mapping,
            self._field_resolution.normalization_version,
        )
