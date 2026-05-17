"""CLI event formatter factories for pipeline execution output.

Provides factory functions that return event handler maps for console
(human-readable) and JSON (structured) output formats. Each factory
returns a dict mapping event types to handler callables, suitable for
subscribing to an EventBus.

Extracted from cli.py to eliminate ~200 lines of duplicated formatters
between _execute_pipeline_with_instances() and _execute_resume_with_instances().
"""

from __future__ import annotations

import json
from collections.abc import Callable

import typer

from elspeth.contracts.cli import ProgressEvent
from elspeth.contracts.events import (
    PhaseCompleted,
    PhaseError,
    PhaseStarted,
    RunCompletionStatus,
    RunSummary,
)
from elspeth.core.events import EventBusProtocol


def create_console_formatters(prefix: str = "Run") -> dict[type, Callable[..., None]]:
    """Create console formatters for human-readable CLI output.

    Args:
        prefix: Label for the summary line (e.g. "Run" or "Resume").
    """

    def _format_phase_started(event: PhaseStarted) -> None:
        target_info = f" → {event.target}" if event.target else ""
        typer.echo(f"[{event.phase.value.upper()}] {event.action.value.capitalize()}{target_info}...")

    def _format_phase_completed(event: PhaseCompleted) -> None:
        duration_str = f"{event.duration_seconds:.2f}s" if event.duration_seconds < 60 else f"{event.duration_seconds / 60:.1f}m"
        typer.echo(f"[{event.phase.value.upper()}] ✓ Completed in {duration_str}")

    def _format_phase_error(event: PhaseError) -> None:
        target_info = f" ({event.target})" if event.target else ""
        typer.echo(f"[{event.phase.value.upper()}] ✗ Error{target_info}: {event.error_message}", err=True)

    def _format_run_summary(event: RunSummary) -> None:
        status_symbols = {
            RunCompletionStatus.COMPLETED: "✓",
            RunCompletionStatus.PARTIAL: "⚠",
            RunCompletionStatus.FAILED: "✗",
            RunCompletionStatus.INTERRUPTED: "⏸",
        }
        symbol = status_symbols[event.status]
        routed_summary = ""
        if (event.routed_success + event.routed_failure) > 0:
            dest_parts = [f"{name}:{count}" for name, count in event.routed_destinations]
            dest_str = ", ".join(dest_parts) if dest_parts else ""
            routed_summary = f" | →{event.routed_success + event.routed_failure:,} routed"
            if dest_str:
                routed_summary += f" ({dest_str})"
        typer.echo(
            f"\n{symbol} {prefix} {event.status.value.upper()}: "
            f"{event.total_rows:,} rows processed | "
            f"✓{event.succeeded:,} succeeded | "
            f"✗{event.failed:,} failed | "
            f"⚠{event.quarantined:,} quarantined"
            f"{routed_summary} | "
            f"{event.duration_seconds:.2f}s total"
        )

    def _format_progress(event: ProgressEvent) -> None:
        rate = event.rows_processed / event.elapsed_seconds if event.elapsed_seconds > 0 else 0
        # elspeth-5069612f3c — show the routed split inline so the streaming
        # CLI signal mirrors the terminal RunSummary line. ``↪`` denotes
        # gate-routed MOVE rows, ``↯`` denotes on_error DIVERT rows.
        typer.echo(
            f"  Processing: {event.rows_processed:,} rows | "
            f"{rate:.0f} rows/sec | "
            f"✓{event.rows_succeeded:,} ✗{event.rows_failed} ⚠{event.rows_quarantined} "
            f"↪{event.rows_routed_success:,} ↯{event.rows_routed_failure:,}"
        )

    return {
        PhaseStarted: _format_phase_started,
        PhaseCompleted: _format_phase_completed,
        PhaseError: _format_phase_error,
        RunSummary: _format_run_summary,
        ProgressEvent: _format_progress,
    }


def create_json_formatters() -> dict[type, Callable[..., None]]:
    """Create JSON formatters for structured CLI output."""

    def _format_phase_started_json(event: PhaseStarted) -> None:
        typer.echo(
            json.dumps(
                {
                    "event": "phase_started",
                    "phase": event.phase.value,
                    "action": event.action.value,
                    "target": event.target,
                }
            )
        )

    def _format_phase_completed_json(event: PhaseCompleted) -> None:
        typer.echo(
            json.dumps(
                {
                    "event": "phase_completed",
                    "phase": event.phase.value,
                    "duration_seconds": event.duration_seconds,
                }
            )
        )

    def _format_phase_error_json(event: PhaseError) -> None:
        typer.echo(
            json.dumps(
                {
                    "event": "phase_error",
                    "phase": event.phase.value,
                    "error": event.error_message,
                    "target": event.target,
                }
            ),
            err=True,
        )

    def _format_run_summary_json(event: RunSummary) -> None:
        typer.echo(
            json.dumps(
                {
                    "event": "run_completed",
                    "run_id": event.run_id,
                    "status": event.status.value,
                    "total_rows": event.total_rows,
                    "succeeded": event.succeeded,
                    "failed": event.failed,
                    "quarantined": event.quarantined,
                    "routed_success": event.routed_success,
                    "routed_failure": event.routed_failure,
                    "routed_destinations": dict(event.routed_destinations),
                    "duration_seconds": event.duration_seconds,
                    "exit_code": event.exit_code,
                }
            )
        )

    def _format_progress_json(event: ProgressEvent) -> None:
        rate = event.rows_processed / event.elapsed_seconds if event.elapsed_seconds > 0 else 0
        # elspeth-5069612f3c — JSON parity with RunSummary terminal output:
        # both rows_routed_success (MOVE) and rows_routed_failure (DIVERT)
        # are emitted on every streaming progress event so a tail-following
        # JSON consumer sees the same taxonomy in-flight as it does at end.
        typer.echo(
            json.dumps(
                {
                    "event": "progress",
                    "rows_processed": event.rows_processed,
                    "rows_succeeded": event.rows_succeeded,
                    "rows_failed": event.rows_failed,
                    "rows_quarantined": event.rows_quarantined,
                    "rows_routed_success": event.rows_routed_success,
                    "rows_routed_failure": event.rows_routed_failure,
                    "elapsed_seconds": event.elapsed_seconds,
                    "rows_per_second": rate,
                }
            )
        )

    return {
        PhaseStarted: _format_phase_started_json,
        PhaseCompleted: _format_phase_completed_json,
        PhaseError: _format_phase_error_json,
        RunSummary: _format_run_summary_json,
        ProgressEvent: _format_progress_json,
    }


def subscribe_formatters(
    event_bus: EventBusProtocol,
    formatters: dict[type, Callable[..., None]],
) -> None:
    """Subscribe all formatters to the event bus.

    Args:
        event_bus: The event bus to subscribe handlers to.
        formatters: Mapping from event type to handler callable.
    """
    for event_type, handler in formatters.items():
        event_bus.subscribe(event_type, handler)
