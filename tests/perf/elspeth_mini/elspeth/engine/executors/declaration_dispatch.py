"""Declaration-contract runtime dispatcher (ADR-010 §Decision 3 + §Semantics).

Adopts **audit-complete with aggregation** posture per ADR-010 §Semantics. On a (row,
call-site) tuple that would violate multiple registered contracts, the
dispatcher DOES NOT short-circuit on the first violation — it collects every
contract's raised audit-evidence exception, attaches dispatcher attribution
to each DCV child, and at loop end:

    * 0 violations → return normally.
    * 1 violation  → raise ``violations[0]`` via reference equality
                     (N6 regression invariant at N=1 — the raise MUST be
                     identity-preserving, NOT an aggregation-of-one wrapper).
    * >=2          → wrap in ``AggregateDeclarationContractViolation`` and
                     raise.

Rationale (see ADR-010 §Semantics amendment):

> ELSPETH's CLAUDE.md "Auditability Standard" makes "I don't know what
> happened" structurally impermissible for any output. Under fail-fast
> first-fire semantics, the audit trail's silence on a second contract's
> evaluation is indistinguishable from "checked and passed" — a Repudiation
> surface (STRIDE) the auditor cannot resolve. Under audit-complete, every
> applicable contract's method runs; every violation is recorded; absence
> in the audit trail means "checked and passed," not "skipped because an
> earlier contract fired."

Four public dispatch sites (H2 §Fix direction):

    * ``run_pre_emission_checks(inputs)`` — before transform.process().
    * ``run_post_emission_checks(inputs, outputs)`` — after process().
    * ``run_batch_flush_checks(inputs, outputs)`` — batch-flush TRANSFORM mode.
    * ``run_boundary_checks(inputs, outputs)`` — source / sink boundary (2C).

A single shared helper (``_dispatch``) implements the collect-then-raise
pattern once; each public function is a thin site-tagged wrapper. This
satisfies F2 §Acceptance "single shared helper with the post-emission
dispatcher, not parallel implementation" — the four sites share ONE
orchestration body.

Catch scope (see ``_dispatch``):

    * ``DeclarationContractViolation`` — primary audit-evidence base.
    * ``PluginContractViolation`` — ``PassThroughContractViolation`` inherits
      this and predates the ADR-010 DCV hierarchy. Catching it here is
      required for audit-complete — otherwise pass-through's raise would
      short-circuit the loop and shadow every later contract on the same
      row. ``PluginContractViolation`` does not support
      ``_attach_contract_name`` (no C4 one-shot flag); the authoritative
      contract name is carried in its 9-key ``to_audit_dict()`` payload
      (``transform`` + ``transform_node_id``) for the single specific
      exception class.

Non-audit-evidence exceptions (plugin bugs raising arbitrary exceptions
from ``applies_to`` or the dispatch method) propagate UNMODIFIED per the
CLAUDE.md plugin-ownership posture — a buggy contract is a framework bug
that must crash.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from typing import Any

from elspeth.contracts.audit_evidence import AuditEvidenceBase
from elspeth.contracts.declaration_contracts import (
    AggregateDeclarationContractViolation,
    BatchFlushInputs,
    BatchFlushOutputs,
    BoundaryInputs,
    BoundaryOutputs,
    DeclarationContract,
    DeclarationContractViolation,
    DispatchSite,
    PostEmissionInputs,
    PostEmissionOutputs,
    PreEmissionInputs,
    _attach_contract_name_from_dispatcher,
    _mark_aggregate_dispatched,
    registered_declaration_contracts_for_site,
)
from elspeth.contracts.errors import FrameworkBugError, PluginContractViolation


def _serialize_plugin_name(plugin: Any) -> str:
    """Return a stable string identifier for the plugin in aggregate messages.

    Direct attribute access for ``name`` — a plugin without ``name`` is a
    framework bug per CLAUDE.md. An empty value is also a framework bug:
    aggregate audit evidence must not fabricate an identifier from the
    plugin's class name when the authoritative ``plugin.name`` is absent.
    """
    name = plugin.name
    if not name:
        raise FrameworkBugError(
            f"Aggregate declaration audit evidence requires a non-empty plugin.name; "
            f"refusing to fabricate an identifier for {type(plugin).__name__} from empty plugin.name"
        )
    return "".join((name,))


def _build_aggregate_message(violations: Sequence[AuditEvidenceBase]) -> str:
    """Compose the aggregate's human-readable message from child messages.

    Structured per-child data travels in ``to_audit_dict()``'s ``violations``
    list; the message is for triage read-ability.
    """
    exception_types = "/".join(type(v).__name__ for v in violations)
    child_preview = "; ".join(str(v) for v in violations[:3])
    if len(violations) > 3:
        child_preview += f"; ... [{len(violations) - 3} more]"
    return f"{len(violations)} declaration contracts fired on a single (row, call-site) tuple ({exception_types}): {child_preview}"


def _dispatch(
    *,
    site: DispatchSite,
    plugin: Any,
    invoke: Callable[[DeclarationContract], None],
) -> None:
    """Collect-then-raise orchestration (shared across all four sites).

    This is the SINGLE dispatcher body. Every public dispatch function is a
    thin wrapper that supplies the site filter and the per-contract invoke
    closure. Adding a new site means adding a new public wrapper and — if
    it carries new bundle types — registering them in declaration_contracts.

    Post-condition: audit-complete — every applicable contract's method has
    been invoked exactly once (or skipped via ``applies_to=False``). A
    raised exception from one contract does NOT short-circuit the loop.
    """
    violations: list[AuditEvidenceBase] = []
    for contract in registered_declaration_contracts_for_site(site):
        if not contract.applies_to(plugin):
            continue
        try:
            invoke(contract)
        except DeclarationContractViolation as exc:
            # C4 closure: attach the authoritative name from the registry
            # entry. Contracts cannot supply contract_name at construction.
            _attach_contract_name_from_dispatcher(exc, contract.name)
            violations.append(exc)
        except PluginContractViolation as exc:
            # PassThroughContractViolation inherits PluginContractViolation
            # (predates the ADR-010 DCV hierarchy — see ADR-010
            # §Consequences → §Neutral). Under audit-complete we must catch
            # it here or it would short-circuit the loop and shadow every
            # later contract on the same row. The exception carries its own
            # 9-key to_audit_dict so the aggregate can surface it without
            # _attach_contract_name.
            violations.append(exc)

    if not violations:
        return
    if len(violations) == 1:
        # Reference-equality fast path. N6 regression invariant asserts
        # both type(raised) is the original concrete subclass AND
        # id(raised) == id(violations[0]) — i.e. no aggregation-of-one
        # wrapper that would break triage SQL filtering by exception_type.
        #
        # AuditEvidenceBase is an ABC that children (DeclarationContractViolation,
        # PluginContractViolation) mix with RuntimeError — every collected
        # violation is a BaseException subclass by construction. mypy cannot
        # narrow this through the ABC, so annotate at the raise site.
        raise violations[0]  # type: ignore[misc]  # AuditEvidenceBase children are RuntimeError subclasses

    aggregate = AggregateDeclarationContractViolation(
        plugin=_serialize_plugin_name(plugin),
        violations=tuple(violations),
        message=_build_aggregate_message(violations),
    )
    _mark_aggregate_dispatched(aggregate)
    raise aggregate


# =============================================================================
# Public dispatch functions — thin site-tagged wrappers
# =============================================================================


def run_pre_emission_checks(inputs: PreEmissionInputs) -> None:
    """Dispatch all applicable pre-emission contracts for ``inputs.plugin``.

    Called from ``TransformExecutor`` before generic input validation and
    before ``transform.process()``. This preserves first-class attribution
    for declaration mismatches when a plugin's ``input_schema`` would also
    reject the same missing field. The current adopter is
    ``DeclaredRequiredFieldsContract``.
    """
    _dispatch(
        site=DispatchSite.PRE_EMISSION,
        plugin=inputs.plugin,
        invoke=lambda contract: contract.pre_emission_check(inputs),
    )


def run_post_emission_checks(
    inputs: PostEmissionInputs,
    outputs: PostEmissionOutputs,
) -> None:
    """Dispatch all applicable post-emission contracts.

    Called from ``TransformExecutor`` after ``transform.process()`` returns
    success. The 2A single-site dispatcher ``run_runtime_checks`` is
    replaced by this function; no legacy alias.
    """
    _dispatch(
        site=DispatchSite.POST_EMISSION,
        plugin=inputs.plugin,
        invoke=lambda contract: contract.post_emission_check(inputs, outputs),
    )


def run_batch_flush_checks(
    inputs: BatchFlushInputs,
    outputs: BatchFlushOutputs,
) -> None:
    """Dispatch all applicable batch-flush contracts.

    Called from ``RowProcessor._cross_check_flush_output`` for TRANSFORM
    mode (ADR-009 §Clause 2 batch-homogeneous semantics). The caller
    supplies ``effective_input_fields`` pre-computed as the intersection of
    every buffered token's contract fields.
    """
    _dispatch(
        site=DispatchSite.BATCH_FLUSH,
        plugin=inputs.plugin,
        invoke=lambda contract: contract.batch_flush_check(inputs, outputs),
    )


def run_boundary_checks(
    inputs: BoundaryInputs,
    outputs: BoundaryOutputs,
) -> None:
    """Dispatch all applicable boundary contracts for one row at a time.

    Current boundary adopters wire this into:
    - ``RowProcessor.process_row()`` after token creation on the source side
    - ``SinkExecutor.write()`` before Layer 2/schema checks on sink paths
    """
    _dispatch(
        site=DispatchSite.BOUNDARY,
        plugin=inputs.plugin,
        invoke=lambda contract: contract.boundary_check(inputs, outputs),
    )
