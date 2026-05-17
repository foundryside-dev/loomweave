"""Entity identifiers and token structures.

These types answer: "How do we refer to things?"
"""

from __future__ import annotations

from dataclasses import dataclass, replace

from elspeth.contracts.schema_contract import PipelineRow


@dataclass(frozen=True, slots=True)
class TokenInfo:
    """Identity and data for a token flowing through the DAG.

    Tokens track row instances through forks/joins:
    - row_id: Stable source row identity
    - token_id: Instance of row in a specific DAG path
    - branch_name: Which fork path this token is on (if forked)
    - fork_group_id: Groups all children from a fork operation
    - join_group_id: Groups all tokens merged in a coalesce operation
    - expand_group_id: Groups all children from an expand operation

    Frozen for immutability - use with_updated_data() to create new instances.
    """

    row_id: str
    token_id: str
    row_data: PipelineRow  # CHANGED from dict[str, Any]
    branch_name: str | None = None
    fork_group_id: str | None = None
    join_group_id: str | None = None
    expand_group_id: str | None = None

    def __post_init__(self) -> None:
        """Validate identity invariants at construction time.

        row_id and token_id are the most fundamental identity fields in
        the system — every audit trail record references them. Empty strings
        would produce valid-looking but meaningless audit entries.
        """
        if not isinstance(self.row_id, str):
            raise TypeError(f"TokenInfo.row_id must be str, got {type(self.row_id).__name__}: {self.row_id!r}")
        if not self.row_id:
            raise ValueError("TokenInfo.row_id must not be empty")
        if not isinstance(self.token_id, str):
            raise TypeError(f"TokenInfo.token_id must be str, got {type(self.token_id).__name__}: {self.token_id!r}")
        if not self.token_id:
            raise ValueError("TokenInfo.token_id must not be empty")
        for _field_name in ("branch_name", "fork_group_id", "join_group_id", "expand_group_id"):
            _value = getattr(self, _field_name)
            if _value is not None:
                if not isinstance(_value, str):
                    raise TypeError(f"TokenInfo.{_field_name} must be str or None, got {type(_value).__name__}: {_value!r}")
                if not _value:
                    raise ValueError(f"TokenInfo.{_field_name} must be None or non-empty string, got {_value!r}")

    def with_updated_data(self, new_data: PipelineRow) -> TokenInfo:
        """Return a new TokenInfo with updated row_data, preserving all lineage fields.

        This method ensures that when row_data is updated after a transform,
        all identity and lineage metadata (branch_name, fork_group_id,
        join_group_id, expand_group_id) are preserved.

        Use this instead of constructing TokenInfo manually when updating
        a token's data after processing.

        Args:
            new_data: The new PipelineRow to use

        Returns:
            A new TokenInfo with the same identity/lineage but new row_data
        """
        return replace(self, row_data=new_data)
