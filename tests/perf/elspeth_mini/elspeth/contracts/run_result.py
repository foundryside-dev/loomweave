"""Pipeline run result — pure data type for pipeline execution outcomes.

Moved to L0 (contracts/) because it has no dependencies above L0: uses only
RunStatus (L0), freeze_fields (L0), and stdlib types. This placement allows
PipelineRunner protocol (also L0) to reference it without a layer violation.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from types import MappingProxyType
from typing import Any

from elspeth.contracts.enums import RunStatus
from elspeth.contracts.freeze import deep_thaw, freeze_fields, require_int


@dataclass(frozen=True, slots=True)
class RunResult:
    """Result of a pipeline run."""

    run_id: str
    status: RunStatus
    rows_processed: int
    rows_succeeded: int
    rows_failed: int
    rows_routed_success: int  # MOVE: gate route_to_sink (intentional success-side routing)
    rows_routed_failure: int  # DIVERT: transform on_error reroute to failure sink
    rows_quarantined: int = 0
    rows_forked: int = 0
    rows_coalesced: int = 0
    rows_coalesce_failed: int = 0  # Coalesce failures (quorum_not_met, incomplete_branches)
    rows_expanded: int = 0  # Deaggregation parent tokens
    rows_buffered: int = 0  # Passthrough mode buffered tokens
    rows_diverted: int = 0  # Rows diverted to failsink during sink write
    routed_destinations: Mapping[str, int] = field(default_factory=lambda: MappingProxyType({}))

    def __post_init__(self) -> None:
        if not self.run_id:
            raise ValueError("run_id must not be empty")
        if not isinstance(self.status, RunStatus):
            raise TypeError(f"RunResult.status must be a RunStatus enum, got {type(self.status).__name__}: {self.status!r}")
        require_int(self.rows_processed, "rows_processed", min_value=0)
        require_int(self.rows_succeeded, "rows_succeeded", min_value=0)
        require_int(self.rows_failed, "rows_failed", min_value=0)
        require_int(self.rows_routed_success, "rows_routed_success", min_value=0)
        require_int(self.rows_routed_failure, "rows_routed_failure", min_value=0)
        require_int(self.rows_quarantined, "rows_quarantined", min_value=0)
        require_int(self.rows_forked, "rows_forked", min_value=0)
        require_int(self.rows_coalesced, "rows_coalesced", min_value=0)
        require_int(self.rows_coalesce_failed, "rows_coalesce_failed", min_value=0)
        require_int(self.rows_expanded, "rows_expanded", min_value=0)
        require_int(self.rows_buffered, "rows_buffered", min_value=0)
        require_int(self.rows_diverted, "rows_diverted", min_value=0)
        freeze_fields(self, "routed_destinations")
        self._check_subset_counter_invariants()
        self._check_status_invariant()

    def _check_subset_counter_invariants(self) -> None:
        """ADR-019: routed/quarantine counters are reporting subsets."""
        if self.rows_routed_success > self.rows_succeeded:
            raise ValueError(
                "RunResult: rows_routed_success must be a subset of rows_succeeded "
                f"(got rows_routed_success={self.rows_routed_success}, rows_succeeded={self.rows_succeeded})"
            )
        if self.rows_routed_failure > self.rows_failed:
            raise ValueError(
                "RunResult: rows_routed_failure must be a subset of rows_failed "
                f"(got rows_routed_failure={self.rows_routed_failure}, rows_failed={self.rows_failed})"
            )
        if self.rows_quarantined > self.rows_failed:
            raise ValueError(
                "RunResult: rows_quarantined must be a subset of rows_failed "
                f"(got rows_quarantined={self.rows_quarantined}, rows_failed={self.rows_failed})"
            )

    def _check_status_invariant(self) -> None:
        """Biconditional invariant linking ``status`` to ADR-019 row counts.

        ``rows_succeeded`` and ``rows_failed`` are exhaustive lifecycle
        predicate counters. Routed/quarantine counters are guard-only subsets.
        ``rows_coalesce_failed`` remains a separate run-level failure signal.

        Non-terminal (``RUNNING``) and signal-bounded (``INTERRUPTED``)
        statuses bypass the predicate.
        """
        success_indicator = self.rows_succeeded > 0
        failure_indicator = self.rows_failed > 0 or self.rows_coalesce_failed > 0

        match (self.status, self.rows_processed, success_indicator, failure_indicator):
            case (RunStatus.RUNNING, _, _, _):
                return
            case (RunStatus.INTERRUPTED, _, _, _):
                return
            case (RunStatus.COMPLETED, _, True, False):
                return
            case (RunStatus.COMPLETED, _, False, _):
                raise ValueError(
                    f"RunResult: status=COMPLETED requires a success indicator "
                    f"(rows_succeeded > 0); "
                    f"got rows_succeeded={self.rows_succeeded} "
                    f"(use status=FAILED when no row reached a success path)"
                )
            case (RunStatus.COMPLETED, _, _, True):
                raise ValueError(
                    f"RunResult: status=COMPLETED requires no failures "
                    f"(rows_failed={self.rows_failed}, "
                    f"rows_coalesce_failed={self.rows_coalesce_failed}); "
                    f"use status=COMPLETED_WITH_FAILURES when at least one row "
                    f"reached a failure terminal state"
                )
            case (RunStatus.COMPLETED_WITH_FAILURES, _, True, True):
                return
            case (RunStatus.COMPLETED_WITH_FAILURES, _, False, _):
                raise ValueError(
                    f"RunResult: status=COMPLETED_WITH_FAILURES requires a success indicator "
                    f"(rows_succeeded > 0); "
                    f"got rows_succeeded={self.rows_succeeded} "
                    f"(use status=FAILED when no row reached a success path)"
                )
            case (RunStatus.COMPLETED_WITH_FAILURES, _, _, False):
                raise ValueError(
                    f"RunResult: status=COMPLETED_WITH_FAILURES requires at least one failure indicator "
                    f"(rows_failed > 0 or rows_coalesce_failed > 0); "
                    f"got rows_failed={self.rows_failed}, "
                    f"rows_coalesce_failed={self.rows_coalesce_failed} "
                    f"(use status=COMPLETED for clean runs)"
                )
            case (RunStatus.FAILED, _, _, _):
                # FAILED has two semantic origins (predicate decision and
                # exception-bounded run) — same biconditional tolerance as
                # before the split.
                return
            case (RunStatus.EMPTY, 0, False, False):
                return
            case (RunStatus.EMPTY, p, _, _) if p > 0:
                raise ValueError(f"RunResult: status=EMPTY requires rows_processed == 0, got rows_processed={p}")
            case (RunStatus.EMPTY, _, True, _):
                raise ValueError(f"RunResult: status=EMPTY requires no success indicator (rows_succeeded={self.rows_succeeded})")
            case (RunStatus.EMPTY, _, _, True):
                raise ValueError(
                    f"RunResult: status=EMPTY requires no failures "
                    f"(rows_failed={self.rows_failed}, "
                    f"rows_coalesce_failed={self.rows_coalesce_failed}); "
                    f"use status=FAILED when the run encountered failures with "
                    f"no successful rows"
                )
            case _:
                raise ValueError(
                    f"RunResult: unhandled status/row-count shape: "
                    f"status={self.status!r}, rows_processed={self.rows_processed}, "
                    f"success_indicator={success_indicator}, "
                    f"failure_indicator={failure_indicator}"
                )

    def to_dict(self) -> dict[str, Any]:
        """Serialize to a plain dict for JSON export.

        Replaces ``dataclasses.asdict()`` which cannot deep-copy
        ``MappingProxyType`` fields (raises ``TypeError: cannot pickle
        'mappingproxy' object``).
        """
        return {
            "run_id": self.run_id,
            "status": self.status.value,
            "rows_processed": self.rows_processed,
            "rows_succeeded": self.rows_succeeded,
            "rows_failed": self.rows_failed,
            "rows_routed_success": self.rows_routed_success,
            "rows_routed_failure": self.rows_routed_failure,
            "rows_quarantined": self.rows_quarantined,
            "rows_forked": self.rows_forked,
            "rows_coalesced": self.rows_coalesced,
            "rows_coalesce_failed": self.rows_coalesce_failed,
            "rows_expanded": self.rows_expanded,
            "rows_buffered": self.rows_buffered,
            "rows_diverted": self.rows_diverted,
            "routed_destinations": deep_thaw(self.routed_destinations),
        }


def derive_terminal_run_status(
    *,
    rows_processed: int,
    rows_succeeded: int,
    rows_failed: int,
    rows_routed_success: int,
    rows_routed_failure: int,
    rows_quarantined: int,
    rows_coalesce_failed: int,
) -> RunStatus:
    """Pick a terminal RunStatus from ADR-019 lifecycle counters.

    success_indicator = rows_succeeded > 0
    failure_indicator = rows_failed > 0 OR rows_coalesce_failed > 0

    Predicate:
    - rows_processed == 0 AND no failure_indicator -> EMPTY (or FAILED if
      a failure indicator is present without source iteration)
    - success_indicator AND not failure_indicator -> COMPLETED
    - success_indicator AND failure_indicator -> COMPLETED_WITH_FAILURES
    - not success_indicator AND rows_processed > 0 -> FAILED

    The result is constrained to the four-value terminal taxonomy
    (COMPLETED / COMPLETED_WITH_FAILURES / FAILED / EMPTY); callers that
    need INTERRUPTED or RUNNING set those values directly.
    """
    if rows_routed_success > rows_succeeded:
        raise ValueError(
            "derive_terminal_run_status: rows_routed_success must be a subset of rows_succeeded "
            f"(got rows_routed_success={rows_routed_success}, rows_succeeded={rows_succeeded})"
        )
    if rows_routed_failure > rows_failed:
        raise ValueError(
            "derive_terminal_run_status: rows_routed_failure must be a subset of rows_failed "
            f"(got rows_routed_failure={rows_routed_failure}, rows_failed={rows_failed})"
        )
    if rows_quarantined > rows_failed:
        raise ValueError(
            "derive_terminal_run_status: rows_quarantined must be a subset of rows_failed "
            f"(got rows_quarantined={rows_quarantined}, rows_failed={rows_failed})"
        )

    success_indicator = rows_succeeded > 0
    failure_indicator = rows_failed > 0 or rows_coalesce_failed > 0
    if rows_processed == 0 and not success_indicator:
        return RunStatus.FAILED if failure_indicator else RunStatus.EMPTY
    if not success_indicator:
        return RunStatus.FAILED
    if failure_indicator:
        return RunStatus.COMPLETED_WITH_FAILURES
    return RunStatus.COMPLETED
