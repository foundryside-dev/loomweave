"""Flow control and edge definitions.

These types answer: "Where does data go next?"
"""

from dataclasses import dataclass
from enum import StrEnum

from elspeth.contracts.enums import RoutingKind, RoutingMode
from elspeth.contracts.errors import RoutingReason
from elspeth.contracts.freeze import freeze_fields
from elspeth.contracts.types import NodeID, SinkName


@dataclass(frozen=True, slots=True)
class RoutingAction:
    """A routing decision from a gate.

    Gates return this to indicate where tokens should go next.
    Use the factory methods to create instances.

    CRITICAL: The `mode` field determines move vs copy semantics:
    - MOVE: Token exits current path, goes to destination only
    - COPY: Token clones to destination AND continues on current path
             (ONLY valid for FORK_TO_PATHS - creates child tokens)

    This field is REQUIRED per architecture. Without it, executors cannot
    correctly record routing events or determine token flow.

    NOTE: COPY mode with ROUTE kind is not supported due to architectural
    constraints (single terminal state per token). Use FORK_TO_PATHS to
    achieve "route to sink and continue" semantics.

    Invariants (enforced by __post_init__):
    - CONTINUE must have empty destinations
    - FORK_TO_PATHS must use COPY mode
    - ROUTE must have exactly one destination
    - ROUTE cannot use COPY mode (use FORK_TO_PATHS instead)
    """

    kind: RoutingKind
    destinations: tuple[str, ...]
    mode: RoutingMode
    reason: RoutingReason | None = None

    def __post_init__(self) -> None:
        """Validate invariants between kind, mode, and destinations."""
        if not isinstance(self.kind, RoutingKind):
            raise TypeError(f"kind must be RoutingKind, got {type(self.kind).__name__}: {self.kind!r}")
        if not isinstance(self.mode, RoutingMode):
            raise TypeError(f"mode must be RoutingMode, got {type(self.mode).__name__}: {self.mode!r}")

        if self.kind == RoutingKind.CONTINUE and self.destinations:
            raise ValueError("CONTINUE must have empty destinations")

        if self.kind == RoutingKind.CONTINUE and self.mode == RoutingMode.COPY:
            raise ValueError("CONTINUE must use MOVE mode, not COPY")

        if self.kind == RoutingKind.FORK_TO_PATHS and self.mode != RoutingMode.COPY:
            raise ValueError("FORK_TO_PATHS must use COPY mode")

        if self.kind == RoutingKind.ROUTE and len(self.destinations) != 1:
            raise ValueError("ROUTE must have exactly one destination")

        if self.kind == RoutingKind.ROUTE and self.mode == RoutingMode.COPY:
            raise ValueError(
                "COPY mode not supported for ROUTE kind. "
                "Use FORK_TO_PATHS to route to sink and continue processing. "
                "Reason: ELSPETH's audit model enforces single terminal state per token; "
                "COPY would require dual terminal states (ROUTED + COMPLETED)."
            )

        freeze_fields(self, "reason")

    @classmethod
    def continue_(cls, *, reason: RoutingReason | None = None) -> "RoutingAction":
        """Continue to next node in pipeline."""
        return cls(
            kind=RoutingKind.CONTINUE,
            destinations=(),
            mode=RoutingMode.MOVE,  # Default for continue
            reason=reason,  # __post_init__ handles defensive copy
        )

    @classmethod
    def route(
        cls,
        label: str,
        *,
        mode: RoutingMode = RoutingMode.MOVE,
        reason: RoutingReason | None = None,
    ) -> "RoutingAction":
        """Route to a specific labeled destination.

        Gates return semantic route labels (e.g., "above", "below", "match").
        The executor resolves these labels via the plugin's `routes` config
        to determine the actual destination (sink name or "continue").

        Args:
            label: Route label that will be resolved via routes config
            mode: MOVE (default). COPY mode not supported - use fork_to_paths() instead.
            reason: Audit trail information about why this route was chosen

        Raises:
            ValueError: If mode is COPY (architectural limitation)
        """
        return cls(
            kind=RoutingKind.ROUTE,
            destinations=(label,),
            mode=mode,
            reason=reason,  # __post_init__ handles defensive copy
        )

    @classmethod
    def fork_to_paths(
        cls,
        paths: list[str],
        *,
        reason: RoutingReason | None = None,
    ) -> "RoutingAction":
        """Fork token to multiple parallel paths (always copy mode).

        Raises:
            ValueError: If paths is empty or contains duplicates.
        """
        if not paths:
            raise ValueError("fork_to_paths requires at least one destination path")
        if len(paths) != len(set(paths)):
            duplicates = [p for p in paths if paths.count(p) > 1]
            raise ValueError(f"fork_to_paths requires unique path names (duplicates: {sorted(set(duplicates))})")
        return cls(
            kind=RoutingKind.FORK_TO_PATHS,
            destinations=tuple(paths),
            mode=RoutingMode.COPY,  # Fork always copies
            reason=reason,  # __post_init__ handles defensive copy
        )


class RouteDestinationKind(StrEnum):
    """Resolved route destination type for gate route labels."""

    CONTINUE = "continue"
    FORK = "fork"
    SINK = "sink"
    PROCESSING_NODE = "processing_node"
    DISCARD = "discard"


@dataclass(frozen=True, slots=True)
class RouteDestination:
    """Resolved destination for a (gate_node_id, route_label) pair."""

    kind: RouteDestinationKind
    sink_name: SinkName | None = None
    next_node_id: NodeID | None = None

    def __post_init__(self) -> None:
        """Validate destination payload by kind."""
        if not isinstance(self.kind, RouteDestinationKind):
            raise TypeError(f"kind must be RouteDestinationKind, got {type(self.kind).__name__}: {self.kind!r}")
        if self.kind == RouteDestinationKind.SINK:
            if self.sink_name is None or not self.sink_name:
                raise ValueError("SINK destination requires non-empty sink_name")
            if self.next_node_id is not None:
                raise ValueError("SINK destination must not include next_node_id")
            return

        if self.kind == RouteDestinationKind.PROCESSING_NODE:
            if self.next_node_id is None or not self.next_node_id:
                raise ValueError("PROCESSING_NODE destination requires non-empty next_node_id")
            if self.sink_name is not None:
                raise ValueError("PROCESSING_NODE destination must not include sink_name")
            return

        if self.sink_name is not None or self.next_node_id is not None:
            raise ValueError(f"{self.kind.value.upper()} destination must not include sink_name or next_node_id")

    @classmethod
    def continue_(cls) -> "RouteDestination":
        return cls(kind=RouteDestinationKind.CONTINUE)

    @classmethod
    def fork(cls) -> "RouteDestination":
        return cls(kind=RouteDestinationKind.FORK)

    @classmethod
    def sink(cls, sink_name: SinkName) -> "RouteDestination":
        return cls(kind=RouteDestinationKind.SINK, sink_name=sink_name)

    @classmethod
    def processing_node(cls, next_node_id: NodeID) -> "RouteDestination":
        return cls(kind=RouteDestinationKind.PROCESSING_NODE, next_node_id=next_node_id)

    @classmethod
    def discard(cls) -> "RouteDestination":
        return cls(kind=RouteDestinationKind.DISCARD)


@dataclass(frozen=True, slots=True)
class RoutingSpec:
    """Specification for a routing edge in the recorded audit trail.

    Strict contract - mode MUST be RoutingMode enum, not string.
    Conversion from DB strings happens in repository layer.
    """

    edge_id: str
    mode: RoutingMode

    def __post_init__(self) -> None:
        """Validate routing spec invariants."""
        if not isinstance(self.mode, RoutingMode):
            raise TypeError(f"mode must be RoutingMode, got {type(self.mode).__name__}: {self.mode!r}")
        if not self.edge_id:
            raise ValueError("RoutingSpec.edge_id must not be empty")


@dataclass(frozen=True, slots=True)
class EdgeInfo:
    """Information about an edge in the execution graph.

    Replaces tuple[str, str, dict[str, Any]] for type safety.
    Strict contract - mode MUST be RoutingMode enum.
    """

    from_node: NodeID
    to_node: NodeID
    label: str
    mode: RoutingMode

    def __post_init__(self) -> None:
        """Validate edge info invariants."""
        if not isinstance(self.mode, RoutingMode):
            raise TypeError(f"mode must be RoutingMode, got {type(self.mode).__name__}: {self.mode!r}")
        if not self.from_node:
            raise ValueError("EdgeInfo.from_node must not be empty")
        if not self.to_node:
            raise ValueError("EdgeInfo.to_node must not be empty")
        if not self.label:
            raise ValueError("EdgeInfo.label must not be empty")
