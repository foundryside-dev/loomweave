"""Dataverse source plugin for ELSPETH.

Loads rows from Microsoft Dataverse entities via OData v4 REST API.
Supports structured OData queries and FetchXML queries with pagination.

IMPORTANT: Sources use allow_coercion=True to normalize external data.
This is the ONLY place in the pipeline where coercion is allowed.
"""

from __future__ import annotations

import re
import urllib.parse
import xml.etree.ElementTree as ET
from collections.abc import Iterator, Mapping
from typing import Any, ClassVar, Self

from pydantic import Field, ValidationError, field_validator, model_validator

import elspeth.contracts.errors as contract_errors
from elspeth.contracts import CallStatus, CallType, Determinism, PluginSchema, SourceRow
from elspeth.contracts.contexts import LifecycleContext, SourceContext
from elspeth.contracts.contract_builder import ContractBuilder
from elspeth.contracts.errors import AuditIntegrityError
from elspeth.contracts.schema_contract_factory import create_contract_from_config
from elspeth.plugins.infrastructure.base import BaseSource
from elspeth.plugins.infrastructure.clients.dataverse import (
    DataverseAuthConfig,
    DataverseClient,
    DataverseClientError,
    DataversePageResponse,
    validate_additional_domain,
)
from elspeth.plugins.infrastructure.config_base import DataPluginConfig
from elspeth.plugins.infrastructure.schema_factory import create_schema_from_config
from elspeth.plugins.sources.field_normalization import (
    FieldResolution,
    normalize_field_name,
    resolve_field_names,
)

# OData annotation prefixes to strip from row data
_ODATA_ANNOTATION_PATTERN = re.compile(r"^@odata\.|@Microsoft\.Dynamics\.CRM\.")
_FORMATTED_VALUE_SUFFIX = "@OData.Community.Display.V1.FormattedValue"


class DataverseSourceConfig(DataPluginConfig):
    """Configuration for Dataverse source plugin.

    Extends DataPluginConfig which requires schema configuration.
    Unlike file-based sources, does not extend PathConfig (no local file path).
    """

    _plugin_component_type: ClassVar[str | None] = "source"

    environment_url: str = Field(
        ...,
        description="Dataverse environment URL (e.g., https://myorg.crm.dynamics.com)",
    )
    auth: DataverseAuthConfig = Field(
        ...,
        description="Authentication configuration",
    )
    api_version: str = Field(
        default="v9.2",
        description="Dataverse Web API version",
    )
    on_validation_failure: str = Field(
        ...,
        description="Sink name for non-conformant rows, or 'discard' for explicit drop",
    )

    # Structured query mode
    entity: str | None = Field(
        default=None,
        description="Entity logical name (e.g., 'contact')",
    )
    select: list[str] | None = Field(
        default=None,
        description="$select fields (None = all)",
    )
    filter: str | None = Field(
        default=None,
        description="$filter expression (static OData only)",
    )
    orderby: str | None = Field(
        default=None,
        description="$orderby expression",
    )
    top: int | None = Field(
        default=None,
        description="$top limit (None = all records)",
    )

    # FetchXML query mode
    fetch_xml: str | None = Field(
        default=None,
        description="Raw FetchXML string",
    )

    # Field handling
    field_mapping: dict[str, str] | None = Field(
        default=None,
        description="Manual field name overrides",
    )
    include_formatted_values: bool = Field(
        default=False,
        description="Preserve Dataverse formatted value annotations",
    )

    # Additional SSRF domain patterns (deployment-level)
    additional_domains: list[str] | None = Field(
        default=None,
        description="Additional Dataverse domain patterns for SSRF allowlist",
    )

    @field_validator("environment_url")
    @classmethod
    def validate_environment_url_https(cls, v: str) -> str:
        """HTTPS required. Bearer tokens sent over plain HTTP would be unencrypted."""
        parsed = urllib.parse.urlparse(v)
        if parsed.scheme != "https":
            raise ValueError(
                f"environment_url must use HTTPS scheme, got {parsed.scheme!r}. "
                f"Bearer tokens are sent in Authorization headers — HTTP would expose them in transit."
            )
        return v

    @field_validator("additional_domains")
    @classmethod
    def validate_additional_domains(cls, v: list[str] | None) -> list[str] | None:
        if v is not None:
            for pattern in v:
                validate_additional_domain(pattern)
        return v

    @model_validator(mode="after")
    def validate_query_mode(self) -> Self:
        has_structured = self.entity is not None
        has_fetchxml = self.fetch_xml is not None
        if has_structured == has_fetchxml:
            raise ValueError("Specify exactly one of: entity (structured query) or fetch_xml")
        if not has_structured and any(f is not None for f in (self.select, self.filter, self.orderby, self.top)):
            raise ValueError("select/filter/orderby/top require entity (structured query mode)")
        return self

    @field_validator("on_validation_failure")
    @classmethod
    def validate_on_validation_failure(cls, v: str) -> str:
        if not v or not v.strip():
            raise ValueError("on_validation_failure must be a sink name or 'discard'")
        return v.strip()

    @field_validator("fetch_xml")
    @classmethod
    def validate_fetch_xml_syntax(cls, v: str | None) -> str | None:
        """Validate FetchXML is parseable XML at config time.

        Structural validation only — broken FetchXML can never produce valid
        results for any row. Analogous to TemplateSyntaxError for Jinja2.
        """
        if v is not None:
            try:
                root = ET.fromstring(v)
            except ET.ParseError as exc:
                raise ValueError(
                    f"fetch_xml contains invalid XML syntax: {exc}. A broken FetchXML query can never produce valid results."
                ) from exc
            if root.tag != "fetch":
                raise ValueError(f"FetchXML root element must be <fetch>, got <{root.tag}>.")
        return v


# Rebuild model to resolve forward references for dynamic module loading
DataverseSourceConfig.model_rebuild()


class DataverseSource(BaseSource):
    """Load rows from Microsoft Dataverse via OData v4 REST API.

    Supports structured OData queries ($select, $filter, $orderby, $top)
    and FetchXML queries with automatic pagination. All responses are
    validated at the Tier 3 boundary (JSON parse, NaN/Infinity rejection).
    """

    name = "dataverse"
    plugin_version = "1.0.0"
    source_file_hash: str | None = "sha256:3ba64a1228760240"
    determinism = Determinism.EXTERNAL_CALL  # Live REST API, not static file read
    config_model = DataverseSourceConfig

    def __init__(self, config: dict[str, Any]) -> None:
        super().__init__(config)
        cfg = DataverseSourceConfig.from_dict(config, plugin_name=self.name)

        # Store config
        self._environment_url = cfg.environment_url
        self._auth_config = cfg.auth
        self._api_version = cfg.api_version
        self._entity = cfg.entity
        self._select = cfg.select
        self._filter = cfg.filter
        self._orderby = cfg.orderby
        self._top = cfg.top
        self._fetch_xml = cfg.fetch_xml
        self._field_mapping = cfg.field_mapping
        self._include_formatted_values = cfg.include_formatted_values
        self._additional_domains = tuple(cfg.additional_domains) if cfg.additional_domains else ()
        self._on_validation_failure = cfg.on_validation_failure

        # Store schema config
        self._schema_config = cfg.schema_config
        self._initialize_declared_guaranteed_fields(self._schema_config)

        # CRITICAL: allow_coercion=True for sources (external data boundary)
        self._schema_class: type[PluginSchema] = create_schema_from_config(
            self._schema_config,
            "DataverseRowSchema",
            allow_coercion=True,
        )
        self.output_schema = self._schema_class

        # Contract setup — Dataverse responses are JSON (no CSV header resolution needed)
        initial_contract = create_contract_from_config(self._schema_config)
        if initial_contract.locked:
            self.set_schema_contract(initial_contract)
            self._contract_builder: ContractBuilder | None = None
        else:
            self._contract_builder = ContractBuilder(initial_contract)

        # Field resolution (populated during load)
        self._field_resolution: FieldResolution | None = None

        # Lazy-constructed client (needs lifecycle context)
        self._client: DataverseClient | None = None

    def on_start(self, ctx: LifecycleContext) -> None:
        """Construct credential and DataverseClient.

        Called before load() — acquires resources from lifecycle context.
        """
        super().on_start(ctx)
        # Construct credential (azure-identity) — validates early
        credential = self._auth_config.create_credential()

        # Obtain rate limiter (with null guard per spec)
        limiter = ctx.rate_limit_registry.get_limiter("dataverse_source") if ctx.rate_limit_registry is not None else None

        # Construct DataverseClient
        self._client = DataverseClient(
            environment_url=self._environment_url,
            credential=credential,
            api_version=self._api_version,
            limiter=limiter,
            additional_domains=self._additional_domains,
        )

    def _validate_entity_exists(self, ctx: SourceContext) -> None:
        """Validate that the configured entity exists in Dataverse metadata.

        Issues a lightweight metadata request to check entity availability.
        Failures are non-fatal — the entity may exist but the metadata
        endpoint may be restricted. Records the probe through the audited
        source call path before continuing or raising.
        """
        if self._client is None:
            raise RuntimeError("on_start() must be called before _validate_entity_exists() — this is a bug")
        if self._entity is None:
            raise RuntimeError("_validate_entity_exists() called outside structured query mode — this is a bug")
        encoded_entity = urllib.parse.quote(self._entity, safe="")
        metadata_url = (
            f"{self._environment_url.rstrip('/')}/api/data/{self._api_version}"
            f"/EntityDefinitions(LogicalName='{encoded_entity}')?$select=LogicalName"
        )
        try:
            page = self._client.get_page(metadata_url)
        except DataverseClientError as e:
            if e.status_code == 404:
                not_found = DataverseClientError(
                    f"Entity '{self._entity}' not found in Dataverse. Check the entity logical name in your pipeline config.",
                    retryable=False,
                    status_code=404,
                    latency_ms=e.latency_ms,
                    error_category=e.error_category,
                    request_url=e.request_url,
                    request_headers=e.request_headers,
                )
                self._record_page_call(
                    ctx,
                    url=not_found.request_url or metadata_url,
                    error=not_found,
                    error_reason=not_found.error_category,
                )
                raise not_found from e
            self._record_page_call(
                ctx,
                url=e.request_url or metadata_url,
                error=e,
                error_reason=e.error_category,
            )
            if e.status_code == 403:
                return
            if e.status_code == 401 and e.retryable:
                self._client.reconstruct_credential(self._auth_config)
            # 5xx, network errors, etc. — re-raise. Silently continuing
            # after a server error means the pipeline proceeds with
            # potentially invalid entity config.
            raise
        self._record_page_call(ctx, url=page.request_url, page=page)

    def _build_query_url(self) -> str:
        """Build the initial OData query URL for structured queries.

        Percent-encodes entity name in the path segment and query parameter
        values to prevent silent corruption from special characters.
        OData $-prefixed parameter names are kept literal (servers require them).
        """
        if self._entity is None:
            raise RuntimeError("_build_query_url() called outside structured query mode — this is a bug")
        encoded_entity = urllib.parse.quote(self._entity, safe="")
        url = f"{self._environment_url.rstrip('/')}/api/data/{self._api_version}/{encoded_entity}"

        params: list[str] = []
        if self._select:
            # Column names are Dataverse identifiers — safe characters
            params.append(f"$select={','.join(self._select)}")
        if self._filter:
            params.append(f"$filter={urllib.parse.quote(self._filter, safe='')}")
        if self._orderby:
            params.append(f"$orderby={urllib.parse.quote(self._orderby, safe='')}")
        if self._top is not None:
            params.append(f"$top={self._top}")

        if params:
            url += "?" + "&".join(params)

        return url

    def _strip_odata_metadata(self, row: dict[str, Any]) -> dict[str, Any]:
        """Strip OData annotations from row data.

        Optionally preserves formatted values as additional fields.

        Args:
            row: Raw row from Dataverse response

        Returns:
            Cleaned row with OData metadata removed
        """
        cleaned: dict[str, Any] = {}
        formatted_values: dict[str, str] = {}

        for key, value in row.items():
            # Preserve formatted values if requested
            if self._include_formatted_values and key.endswith(_FORMATTED_VALUE_SUFFIX):
                # Extract field name: "statecode@OData.Community.Display.V1.FormattedValue" → "statecode"
                field_name = key[: -len(_FORMATTED_VALUE_SUFFIX)]
                formatted_values[field_name] = value
                continue

            # Strip OData annotations
            if _ODATA_ANNOTATION_PATTERN.match(key):
                continue

            cleaned[key] = value

        # Merge formatted values with __formatted suffix
        if self._include_formatted_values:
            for field_name, formatted in formatted_values.items():
                formatted_key = f"{field_name}__formatted"
                # Collision detection
                if formatted_key in cleaned:
                    raise ValueError(
                        f"Formatted value field name collision: both '{formatted_key}' and "
                        f"'{field_name}' with formatted value annotation exist in the entity. "
                        f"Disable include_formatted_values or rename the conflicting field."
                    )
                cleaned[formatted_key] = formatted

        return cleaned

    def _normalize_row_fields(
        self,
        row: dict[str, Any],
        is_first_row: bool,
    ) -> dict[str, Any]:
        """Normalize field names and apply field mapping.

        On first row, creates the field resolution mapping.

        Args:
            row: Row with original field names
            is_first_row: Whether this is the first row being processed

        Returns:
            Row with normalized field names
        """
        if is_first_row or self._field_resolution is None:
            raw_headers = list(row.keys())
            self._field_resolution = resolve_field_names(
                raw_headers=raw_headers,
                field_mapping=self._field_mapping,
                columns=None,
            )

        # Apply resolution mapping.
        # Tier 3: Dataverse responses may include fields not in the first row
        # (sparse entities, dynamic attributes). Normalize new fields using the
        # same algorithm rather than passing them through raw — inconsistent
        # normalization would break downstream template references.
        # Schema validation (lines below) quarantines rows with unexpected fields.
        mapping = self._field_resolution.resolution_mapping
        result: dict[str, Any] = {}
        has_unmapped_fields = False
        for k, v in row.items():
            normalized_name = mapping.get(k)
            if normalized_name is None:
                # Field not in initial mapping — normalize individually
                normalized_name = normalize_field_name(k)
                has_unmapped_fields = True
            result[normalized_name] = v

        # If this row had fields not in the initial mapping, rebuild
        # field resolution from this row's complete key set. This ensures
        # the resolution mapping used for contract inference covers all
        # fields, not just those from the (possibly quarantined) first row.
        if has_unmapped_fields:
            self._field_resolution = resolve_field_names(
                raw_headers=list(row.keys()),
                field_mapping=self._field_mapping,
                columns=None,
            )

        return result

    def _record_page_call(
        self,
        ctx: SourceContext,
        *,
        url: str,
        page: DataversePageResponse | None = None,
        error: DataverseClientError | None = None,
        error_reason: str | None = None,
    ) -> None:
        """Record a page fetch in the audit trail.

        PluginContext.record_call() handles telemetry after the successful
        audit write, preserving the shared audit-first contract.

        Args:
            ctx: Source context for record_call
            url: URL that was requested
            page: Response page (for success)
            error: DataverseClientError (for errors)
            error_reason: Additional error context
        """
        if page is not None:
            request_data = {
                "method": "GET",
                "url": url,
                "headers": page.request_headers,  # Already fingerprinted by client
            }
            response_data = {
                "status_code": page.status_code,
                "row_count": len(page.rows),
                "headers": page.headers,
                "next_link": page.next_link,
                "paging_cookie": page.paging_cookie,
                "more_records": page.more_records,
            }
            try:
                ctx.record_call(
                    call_type=CallType.HTTP,
                    status=CallStatus.SUCCESS,
                    request_data=request_data,
                    response_data=response_data,
                    latency_ms=page.latency_ms,
                    provider="dataverse",
                )
            except contract_errors.TIER_1_ERRORS:
                raise
            except Exception as exc:
                raise AuditIntegrityError(
                    f"Failed to record successful page fetch to audit trail "
                    f"(url={url!r}). "
                    f"Page fetch completed but audit record is missing."
                ) from exc
        elif error is not None:
            request_data = {"method": "GET", "url": url}
            error_data = {
                "error_type": type(error).__name__,
                "message": str(error),
                "status_code": error.status_code,
                "reason": error_reason,
            }
            try:
                ctx.record_call(
                    call_type=CallType.HTTP,
                    status=CallStatus.ERROR,
                    request_data=request_data,
                    error=error_data,
                    latency_ms=error.latency_ms,
                    provider="dataverse",
                )
            except contract_errors.TIER_1_ERRORS:
                raise
            except Exception as exc:
                raise AuditIntegrityError(
                    f"Failed to record error page fetch to audit trail "
                    f"(url={url!r}, error={error!r}). "
                    f"Error occurred but audit record is missing."
                ) from exc

    def load(self, ctx: SourceContext) -> Iterator[SourceRow]:
        """Load rows from Dataverse via OData pagination.

        Yields SourceRow.valid() for validated rows and
        SourceRow.quarantined() for rows failing schema validation.
        """
        # Instance-level flag — NOT reset per page (spec: schema lock scoping)
        self._first_valid_row_processed = False
        is_first_row = True
        pages_fetched = 0
        rows_yielded = 0
        quarantine_count = 0

        # Client must be constructed by on_start() before load()
        if self._client is None:
            raise RuntimeError("on_start() must be called before load() — this is a bug")

        # Track the last URL seen — used in the error path where we don't
        # have a page response but need the actual URL for audit accuracy.
        last_fetched_url: str = self._build_query_url() if self._entity else "(FetchXML)"

        if self._entity is not None:
            self._validate_entity_exists(ctx)

        try:
            if self._entity is not None:
                # Structured OData query
                url = self._build_query_url()
                page_iterator = self._client.paginate_odata(url)
            else:
                # FetchXML query
                if self._fetch_xml is None:
                    raise RuntimeError("config validator ensures entity or fetch_xml — neither is set, this is a bug")
                # Extract entity name from FetchXML
                root = ET.fromstring(self._fetch_xml)
                entity_elem = root.find("entity")
                if entity_elem is None:
                    raise RuntimeError("FetchXML is missing <entity> element — cannot determine entity name for URL")
                entity_name = entity_elem.get("name")
                if entity_name is None:
                    raise RuntimeError("FetchXML <entity> element missing 'name' attribute")
                page_iterator = self._client.paginate_fetchxml(entity_name, self._fetch_xml)

            for page in page_iterator:
                pages_fetched += 1
                last_fetched_url = page.request_url

                # Record successful page fetch — use the actual URL from the
                # response DTO, not the rebuilt initial URL. For pages 2+, the
                # actual URL is the nextLink from the previous page.
                self._record_page_call(ctx, url=page.request_url, page=page)

                # Process rows
                for raw_row in page.rows:
                    # Strip OData metadata
                    try:
                        cleaned_row = self._strip_odata_metadata(raw_row)
                    except ValueError as e:
                        # Formatted value collision — quarantine
                        quarantine_count += 1
                        ctx.record_validation_error(
                            row=raw_row,
                            error=str(e),
                            schema_mode="odata_strip",
                            destination=self._on_validation_failure,
                        )
                        if self._on_validation_failure != "discard":
                            yield SourceRow.quarantined(
                                row=raw_row,
                                error=str(e),
                                destination=self._on_validation_failure,
                            )
                        continue

                    # Normalize field names — Tier 3 boundary: field names from
                    # Dataverse can be arbitrary strings. normalize_field_name()
                    # raises ValueError if a name normalizes to empty string
                    # (e.g., all-special-character field names). Quarantine the
                    # row rather than crashing the entire load.
                    try:
                        normalized_row = self._normalize_row_fields(cleaned_row, is_first_row)
                    except ValueError as e:
                        quarantine_count += 1
                        ctx.record_validation_error(
                            row=cleaned_row,
                            error=f"Field normalization failed: {e}",
                            schema_mode="field_normalization",
                            destination=self._on_validation_failure,
                        )
                        if self._on_validation_failure != "discard":
                            yield SourceRow.quarantined(
                                row=cleaned_row,
                                error=f"Field normalization failed: {e}",
                                destination=self._on_validation_failure,
                            )
                        continue
                    is_first_row = False

                    # Validate against schema
                    try:
                        validated = self._schema_class.model_validate(normalized_row)
                        validated_row = validated.to_row()
                    except ValidationError as e:
                        quarantine_count += 1
                        ctx.record_validation_error(
                            row=normalized_row,
                            error=str(e),
                            schema_mode="validation",
                            destination=self._on_validation_failure,
                        )
                        if self._on_validation_failure != "discard":
                            yield SourceRow.quarantined(
                                row=normalized_row,
                                error=str(e),
                                destination=self._on_validation_failure,
                            )
                        continue

                    # Lock contract on first valid row (FLEXIBLE/OBSERVED)
                    if not self._first_valid_row_processed and self._contract_builder is not None:
                        resolution_map: Mapping[str, str]
                        if self._field_resolution is not None:
                            resolution_map = self._field_resolution.resolution_mapping
                        else:
                            resolution_map = {k: k for k in validated_row}

                        self._contract_builder.process_first_row(validated_row, resolution_map)
                        self.set_schema_contract(self._contract_builder.contract)
                        self._first_valid_row_processed = True

                    # Validate against locked contract to catch type drift on
                    # inferred fields. Pydantic extra="allow" accepts any type
                    # for extras — the contract enforces inferred types here.
                    contract = self._contract_builder.contract if self._contract_builder is not None else self.require_schema_contract()
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

                    rows_yielded += 1
                    yield SourceRow.valid(validated_row, contract=contract)

        except DataverseClientError as e:
            # Use the actual failing URL from the error when available
            # (carried on DataverseClientError since the request metadata fix).
            # Falls back to last_fetched_url for errors raised outside
            # _execute_request (e.g. SSRF validation before the HTTP call).
            error_url = e.request_url or last_fetched_url
            self._record_page_call(
                ctx,
                url=error_url,
                error=e,
                error_reason=e.error_category,
            )
            # 401 with retryable=True: reconstruct credential before engine retry
            if e.status_code == 401 and e.retryable:
                if self._client is None:
                    raise RuntimeError("on_start() must be called before load() — this is a bug") from None
                self._client.reconstruct_credential(self._auth_config)
            raise

        # Force-lock contract if no valid rows were yielded across ALL pages
        if not self._first_valid_row_processed and self._contract_builder is not None:
            self.set_schema_contract(self._contract_builder.contract.with_locked())

        # Store counters for on_complete telemetry
        self._pages_fetched = pages_fetched
        self._rows_yielded = rows_yielded
        self._quarantine_count = quarantine_count

    def on_complete(self, ctx: LifecycleContext) -> None:
        """Source statistics available via stored counters.

        No SourceCompleted telemetry event type exists yet (AzureBlobSource
        has the same gap). Counters are stored as instance attributes by
        load() for diagnostic access. When a SourceCompleted event is added
        to contracts/events.py, emit here via ctx.telemetry_emit.
        """
        super().on_complete(ctx)
        # Counters stored by load(): _pages_fetched, _rows_yielded, _quarantine_count

    def get_field_resolution(self) -> tuple[Mapping[str, str], str | None] | None:
        """Return field normalization mapping for audit trail recovery."""
        if self._field_resolution is None:
            return None
        return (
            self._field_resolution.resolution_mapping,
            self._field_resolution.normalization_version,
        )

    def close(self) -> None:
        """Release DataverseClient resources."""
        if self._client is not None:
            self._client.close()
            self._client = None
