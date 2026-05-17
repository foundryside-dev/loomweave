"""All status codes, modes, and kinds used across subsystem boundaries.

CRITICAL: Every plugin MUST declare a Determinism value at registration.
There is no "unknown" - undeclared determinism crashes at registration time.
This is per ELSPETH's principle: "I don't know what happened" is never acceptable.
"""

from enum import StrEnum


class RunStatus(StrEnum):
    """Status of a pipeline run.

    Stored in the database (runs.status).

    The four-value terminal taxonomy (COMPLETED / COMPLETED_WITH_FAILURES /
    FAILED / EMPTY) was introduced in Phase 2.2 (elspeth-0de989c56d) so an
    operator scanning ``/api/runs/{rid}`` can distinguish "ran cleanly" from
    "ran but no row succeeded" without reading diagnostics.  RUNNING is
    non-terminal; INTERRUPTED is signal-bounded (SIGINT/SIGTERM).

    The presence-indicator predicate that maps row-count shapes to status
    values is enforced in :class:`elspeth.contracts.run_result.RunResult`'s
    ``__post_init__`` (the in-memory engine record carries the row counters).
    The web sessions DB and the Pydantic API schemas mirror the same
    invariant; the Landscape audit ``Run`` dataclass has no row-count
    fields so the enum widening alone is sufficient at that layer.
    """

    RUNNING = "running"
    COMPLETED = "completed"
    COMPLETED_WITH_FAILURES = "completed_with_failures"
    FAILED = "failed"
    EMPTY = "empty"
    INTERRUPTED = "interrupted"


class NodeStateStatus(StrEnum):
    """Status of a node processing a token.

    Stored in database (node_states.status).
    """

    OPEN = "open"
    PENDING = "pending"
    COMPLETED = "completed"
    FAILED = "failed"


class ExportStatus(StrEnum):
    """Status of run export operation.

    Stored in the database.
    """

    PENDING = "pending"
    COMPLETED = "completed"
    FAILED = "failed"


class BatchStatus(StrEnum):
    """Status of an aggregation batch.

    Stored in database (batches.status).
    """

    DRAFT = "draft"
    EXECUTING = "executing"
    COMPLETED = "completed"
    FAILED = "failed"


class TriggerType(StrEnum):
    """Type of trigger that caused an aggregation batch to execute.

    Stored in database (batches.trigger_type).

    Values:
        COUNT: Batch reached configured row count threshold
        TIMEOUT: Batch reached configured time limit
        CONDITION: Custom condition expression evaluated to true
        END_OF_SOURCE: Source exhausted, flush remaining rows
    """

    COUNT = "count"
    TIMEOUT = "timeout"
    CONDITION = "condition"
    END_OF_SOURCE = "end_of_source"


class NodeType(StrEnum):
    """Type of node in the execution graph.

    Stored in database (nodes.node_type).
    """

    SOURCE = "source"
    TRANSFORM = "transform"
    GATE = "gate"
    AGGREGATION = "aggregation"
    COALESCE = "coalesce"
    SINK = "sink"


class Determinism(StrEnum):
    """Plugin determinism classification for reproducibility.

    Every plugin MUST declare one of these at registration. No default.
    Undeclared determinism = crash at registration time.

    Each value tells you what to do for replay/verify:
    - DETERMINISTIC: Just re-run, expect identical output
    - SEEDED: Capture seed, replay with same seed
    - IO_READ: Capture what was read (time, files, env)
    - IO_WRITE: Be careful - has side effects on replay
    - EXTERNAL_CALL: Record request/response for replay
    - NON_DETERMINISTIC: Must record output, cannot reproduce

    Stored in database (nodes.determinism).
    """

    DETERMINISTIC = "deterministic"
    SEEDED = "seeded"
    IO_READ = "io_read"
    IO_WRITE = "io_write"
    EXTERNAL_CALL = "external_call"
    NON_DETERMINISTIC = "non_deterministic"


class RoutingKind(StrEnum):
    """Kind of routing action from a gate.

    Stored in routing_events.
    """

    CONTINUE = "continue"
    ROUTE = "route"
    FORK_TO_PATHS = "fork_to_paths"


class RoutingMode(StrEnum):
    """Mode for routing edges.

    MOVE: Token exits current path, goes to destination only
    COPY: Token clones to destination AND continues on current path
    DIVERT: Token is diverted from normal flow to error/quarantine sink.
            Like MOVE, but semantically distinct: represents failure handling,
            not intentional routing. Used for source quarantine and transform
            on_error edges. These are structural markers in the DAG — rows
            reach these sinks via exception handling, not by traversing the edge.

    Stored in the database.
    """

    MOVE = "move"
    COPY = "copy"
    DIVERT = "divert"


# ADR-019 (two-axis terminal model): TerminalOutcome and TerminalPath split the
# terminal state into a lifecycle answer (outcome) and a provenance answer
# (path).  See ``docs/architecture/adr/019-two-axis-terminal-model.md``
# § "Counter derivation contract — public API field names preserved" (round-4
# amendment, 2026-05-04) for the normative ``(outcome, path) → counter
# increment`` mapping.
class TerminalOutcome(StrEnum):
    """Lifecycle answer for a row that has reached a terminal state.

    ADR-019 § Decision: when ``completed=True``, ``outcome`` is one of three
    values; when ``completed=False`` (only ``BUFFERED`` today), ``outcome`` is
    NULL.  ``SUCCESS`` and ``FAILURE`` are predicate inputs to
    ``RunResult.__post_init__``'s ``RunStatus`` derivation; ``TRANSIENT`` is
    explicitly NOT a predicate input — it marks parent-token bookkeeping
    (``FORK_PARENT``, ``EXPAND_PARENT``), batch absorption (``BATCH_CONSUMED``),
    and sink-fallback-to-failsink absorptions whose lifecycle answers live on
    a paired ``token_outcomes`` row, ``node_state``, or ``artifacts`` row
    elsewhere.

    See ADR-019 § "Why TRANSIENT exists as a third outcome value" for the
    rationale that admits this third value.
    """

    SUCCESS = "success"
    FAILURE = "failure"
    TRANSIENT = "transient"


class TerminalPath(StrEnum):
    """Provenance answer for a row's terminal — how did it get there?

    Producer-declared, producer-emitted; never inferred from graph topology
    or counter context.  See ADR-019 § "Classification is producer-declared,
    not topology-derivable" — ``ON_ERROR_ROUTED`` and
    ``SINK_FALLBACK_TO_FAILSINK`` are structurally identical at the audit
    layer (both write a paired ``NodeStateStatus.COMPLETED`` ``node_state``
    plus an ``artifacts`` row at a different node), so only the producer
    knows whether the lifecycle answer is FAILURE (transform threw, on-error
    sink received) or TRANSIENT (sink-write fallback for visibility).

    Stored alongside ``TerminalOutcome`` in the post-Stage-2 ``token_outcomes``
    schema.  ``BUFFERED`` is the only non-terminal path — it pairs with
    ``outcome IS NULL`` to mark a row that hasn't decided yet.
    """

    DEFAULT_FLOW = "default_flow"
    GATE_ROUTED = "gate_routed"
    GATE_DISCARDED = "gate_discarded"
    ON_ERROR_ROUTED = "on_error_routed"
    FILTER_DROPPED = "filter_dropped"
    COALESCED = "coalesced"
    UNROUTED = "unrouted"
    QUARANTINED_AT_SOURCE = "quarantined_at_source"
    SINK_FALLBACK_TO_FAILSINK = "sink_fallback_to_failsink"
    SINK_DISCARDED = "sink_discarded"
    FORK_PARENT = "fork_parent"
    EXPAND_PARENT = "expand_parent"
    BATCH_CONSUMED = "batch_consumed"
    BUFFERED = "buffered"


# Closed-set partition over the cross-product of TerminalOutcome and
# TerminalPath per the ADR-019 mapping table at lines 99-115.  Every legal
# terminal pair is enumerated below; the assertion that follows verifies every
# ``TerminalPath`` is either covered by a legal terminal pair OR present in
# ``_NON_TERMINAL_PATHS``.
_LEGAL_TERMINAL_PAIRS: frozenset[tuple[TerminalOutcome, TerminalPath]] = frozenset(
    {
        (TerminalOutcome.SUCCESS, TerminalPath.DEFAULT_FLOW),
        (TerminalOutcome.SUCCESS, TerminalPath.GATE_ROUTED),
        (TerminalOutcome.SUCCESS, TerminalPath.GATE_DISCARDED),
        (TerminalOutcome.FAILURE, TerminalPath.ON_ERROR_ROUTED),
        (TerminalOutcome.SUCCESS, TerminalPath.FILTER_DROPPED),
        (TerminalOutcome.SUCCESS, TerminalPath.COALESCED),
        (TerminalOutcome.FAILURE, TerminalPath.UNROUTED),
        (TerminalOutcome.FAILURE, TerminalPath.QUARANTINED_AT_SOURCE),
        (TerminalOutcome.TRANSIENT, TerminalPath.SINK_FALLBACK_TO_FAILSINK),
        (TerminalOutcome.FAILURE, TerminalPath.SINK_DISCARDED),
        (TerminalOutcome.TRANSIENT, TerminalPath.FORK_PARENT),
        (TerminalOutcome.TRANSIENT, TerminalPath.EXPAND_PARENT),
        (TerminalOutcome.TRANSIENT, TerminalPath.BATCH_CONSUMED),
    }
)

_NON_TERMINAL_PATHS: frozenset[TerminalPath] = frozenset(
    {
        TerminalPath.BUFFERED,
    }
)


# Exhaustiveness: every TerminalPath value MUST be covered by either a legal
# terminal pair (paired with some TerminalOutcome) or the non-terminal set.
# An unclassified path would silently land in the ``case _:`` arm of any
# future (outcome, path) match in the recorder/accumulator and corrupt the
# audit-integrity invariant the way an unclassified terminal value would.
_paths_in_terminal_pairs: frozenset[TerminalPath] = frozenset(path for _, path in _LEGAL_TERMINAL_PAIRS)
_all_terminal_paths: frozenset[TerminalPath] = frozenset(TerminalPath)
_classified_terminal_paths: frozenset[TerminalPath] = _paths_in_terminal_pairs | _NON_TERMINAL_PATHS
if _classified_terminal_paths != _all_terminal_paths:
    _unclassified_paths = _all_terminal_paths - _classified_terminal_paths
    raise AssertionError(
        f"TerminalPath members {sorted(p.name for p in _unclassified_paths)} are "
        f"not classified into _LEGAL_TERMINAL_PAIRS or _NON_TERMINAL_PATHS in "
        f"contracts/enums.py — every new TerminalPath value must be added to "
        f"exactly one (paired with a TerminalOutcome in _LEGAL_TERMINAL_PAIRS, "
        f"or listed alone in _NON_TERMINAL_PATHS)."
    )

# Mutual exclusion: a path cannot be both terminal-paired and non-terminal.
_paths_overlap = _paths_in_terminal_pairs & _NON_TERMINAL_PATHS
if _paths_overlap:
    raise AssertionError(
        f"TerminalPath members {sorted(p.name for p in _paths_overlap)} appear in "
        f"BOTH _LEGAL_TERMINAL_PAIRS and _NON_TERMINAL_PATHS — these sets must be "
        f"disjoint (a path is either terminal-paired or non-terminal, never both)."
    )

# Outcome exhaustiveness: every TerminalOutcome value MUST be the lifecycle
# answer for at least one legal terminal pair.  An unused outcome would mean
# the enum has dead values that no producer can emit — drift from the ADR.
_outcomes_in_terminal_pairs: frozenset[TerminalOutcome] = frozenset(outcome for outcome, _ in _LEGAL_TERMINAL_PAIRS)
_all_terminal_outcomes: frozenset[TerminalOutcome] = frozenset(TerminalOutcome)
if _outcomes_in_terminal_pairs != _all_terminal_outcomes:
    _orphaned_outcomes = _all_terminal_outcomes - _outcomes_in_terminal_pairs
    raise AssertionError(
        f"TerminalOutcome members {sorted(o.name for o in _orphaned_outcomes)} "
        f"do not appear in any pair in _LEGAL_TERMINAL_PAIRS in contracts/enums.py "
        f"— every TerminalOutcome value must be the lifecycle answer for at least "
        f"one legal (outcome, path) pair per the ADR-019 mapping table."
    )


class CallType(StrEnum):
    """Type of external call (Phase 6).

    Stored in database (calls.call_type).
    """

    LLM = "llm"
    HTTP = "http"
    HTTP_REDIRECT = "http_redirect"
    SQL = "sql"
    VECTOR = "vector"
    FILESYSTEM = "filesystem"


class CallStatus(StrEnum):
    """Status of an external call (Phase 6).

    Stored in database (calls.status).
    """

    SUCCESS = "success"
    ERROR = "error"


class RunMode(StrEnum):
    """Pipeline execution mode for live/replay/verify behavior.

    Stored in database (runs.run_mode).

    Values:
        LIVE: Make real API calls, record everything
        REPLAY: Use recorded responses, skip live calls
        VERIFY: Make real calls, compare to recorded
    """

    LIVE = "live"
    REPLAY = "replay"
    VERIFY = "verify"


class TelemetryGranularity(StrEnum):
    """Granularity of telemetry events emitted by the TelemetryManager.

    Values:
        LIFECYCLE: Only run start/complete/failed events (minimal overhead)
        ROWS: Lifecycle + row-level events (row_started, row_completed, etc.)
        FULL: Rows + external call events (LLM requests, HTTP calls, etc.)
    """

    LIFECYCLE = "lifecycle"
    ROWS = "rows"
    FULL = "full"


class BackpressureMode(StrEnum):
    """How to handle backpressure when telemetry exporters can't keep up.

    Values:
        BLOCK: Block the pipeline until exporters catch up (safest, may slow pipeline)
        DROP: Drop events when buffer is full (lossy, no pipeline impact)
        SLOW: Adaptive rate limiting (not yet implemented)
    """

    BLOCK = "block"
    DROP = "drop"
    SLOW = "slow"


# Backpressure modes that are currently implemented.
# Used by RuntimeTelemetryConfig.from_settings() to fail fast on unimplemented modes.
_IMPLEMENTED_BACKPRESSURE_MODES = frozenset({BackpressureMode.BLOCK, BackpressureMode.DROP})


class ReproducibilityGrade(StrEnum):
    """Reproducibility levels for a completed run.

    Grades:
    - FULL_REPRODUCIBLE: All nodes are deterministic or seeded. The run can be
      fully re-executed with identical results (given the same seed).
    - REPLAY_REPRODUCIBLE: At least one node is nondeterministic (e.g., LLM calls).
      Results can only be replayed using recorded external call responses.
    - ATTRIBUTABLE_ONLY: Payloads have been purged. We can verify what happened
      via hashes, but cannot replay the run.

    Stored in database (runs.reproducibility_grade).
    """

    FULL_REPRODUCIBLE = "full_reproducible"
    REPLAY_REPRODUCIBLE = "replay_reproducible"
    ATTRIBUTABLE_ONLY = "attributable_only"


class OutputMode(StrEnum):
    """Output mode for aggregation batches.

    Stored in database.

    Values:
        PASSTHROUGH: Emit buffered rows unchanged after flush
        TRANSFORM: Emit transformed output from aggregation plugin
    """

    PASSTHROUGH = "passthrough"
    TRANSFORM = "transform"


def error_edge_label(transform_id: str) -> str:
    """Canonical label for a transform error DIVERT edge.

    Shared between DAG construction (dag.py) and error-routing audit recording
    (executors.py, processor.py) to prevent label drift.

    Args:
        transform_id: Stable transform name for error-route labels.
    """
    return f"__error_{transform_id}__"
