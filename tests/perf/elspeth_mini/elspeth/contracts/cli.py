"""CLI-related type contracts."""

from dataclasses import dataclass
from typing import NotRequired, TypedDict

from elspeth.contracts.enums import RunStatus


@dataclass(frozen=True, slots=True)
class ProgressEvent:
    """Progress event emitted during pipeline execution.

    Emitted every N rows (default 100) to provide visibility into long-running
    pipelines. The CLI subscribes to these events and renders progress output.

    elspeth-5069612f3c — ``rows_routed`` is split into MOVE (intentional gate
    ``route_to_sink``) and DIVERT (transform ``on_error`` reroute) buckets so
    the in-flight progress signal mirrors the terminal-state taxonomy. All
    counters are REQUIRED at construction time: per CLAUDE.md fabrication
    test, defaulting an absent value to ``0`` would make "we don't know"
    indistinguishable from "definitely zero" on the wire. The engine
    emitters at ``orchestrator/core.py`` already populate every field on
    every emission; making them required crashes any future drift loudly
    at the producer site rather than silently substituting ``0`` downstream.

    Attributes:
        rows_processed: Total rows processed so far.
        rows_succeeded: Rows that completed successfully (success-sink path).
        rows_failed: Rows that failed processing.
        rows_quarantined: Rows that were quarantined for investigation.
        rows_routed_success: Rows redirected by gate ``route_to_sink`` MOVE.
        rows_routed_failure: Rows redirected by transform ``on_error`` DIVERT.
        elapsed_seconds: Time elapsed since run started.
    """

    rows_processed: int
    rows_succeeded: int
    rows_failed: int
    rows_quarantined: int
    elapsed_seconds: float
    rows_routed_success: int
    rows_routed_failure: int


class ExecutionResult(TypedDict):
    """Result from pipeline execution.

    Returned by _execute_pipeline_with_instances() in cli.py.

    Required fields:
        run_id: Unique identifier for this pipeline run.
        status: Execution status (e.g., "completed", "failed").
        rows_processed: Total number of rows processed.

    Optional fields (may be added for detailed reporting):
        rows_succeeded: Number of rows that completed successfully.
        rows_failed: Number of rows that failed processing.
        duration_seconds: Total execution time in seconds.
    """

    run_id: str
    status: RunStatus  # Strict: enum (str subclass) instead of naked string
    rows_processed: int
    rows_succeeded: NotRequired[int]
    rows_failed: NotRequired[int]
    duration_seconds: NotRequired[float]
