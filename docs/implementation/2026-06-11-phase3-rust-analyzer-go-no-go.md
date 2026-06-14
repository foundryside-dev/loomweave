# Phase 3 (rust-analyzer enrichment) — go / no-go memo (2026-06-11)

Non-normative recommendation for the decider to ratify. **This memo does not
start Phase 3.** It weighs whether the rust-analyzer edge-confidence enrichment
recorded as admissible-but-deferred in ADR-049 §2 and the Rust plugin spec §6
("Phase 3") should be **BUILT**, **DEFERRED behind a demand threshold**, or
**DROPPED**.

## Context

The shipped Rust plugin parses with `syn` (ADR-049 §2 — bound by the project's
network-free / credential-free `analyze` posture: no `cargo metadata`, no
toolchain). The `calls` edge MVP is honest-by-design: it resolves only what the
syntactic backend can resolve, and emits *no* edge (an unresolved call *site*
instead) rather than fabricating one. rust-analyzer was recorded as the **only**
admissible path to deeper resolution — and **only** as strictly-additive
edge-confidence enrichment that reproduces the §1 qualname byte-for-byte.

## Demand evidence (Sprint-3 scale QA, 2026-06-10, quoted)

The syn `calls` MVP resolves a *small fraction* of call sites on foreign code —
this unresolved remainder is the honest demand signal for a second engine:

- **tokio** (`2e7930fe`): **50 resolved `calls` edges** over **7 935 entities**
  / 8 008 total edges. The QA report: "the `calls` MVP resolves a small
  fraction on foreign code (e.g. tokio: 50 resolved calls edges)".
- **rust-analyzer corpus** (`587ce15e`): 30 369 entities, 41 091 edges; resolved
  `calls` "stats in `/tmp` harvests" — same small-fraction envelope.
- Documented envelope: "bare same-module calls and method calls land as
  unresolved sites, never fabricated edges."

So the demand is real and large in *absolute* terms (most call sites on foreign
code are unresolved). It is **not** the top dogfood pain, though: the live
Sprint-3 defect queue is dominated by *correctness* gaps the MVP already
surfaces honestly — cfg-twin method collisions (`clarion-dfeb905f46`), the
reserved-`:` file-drop (`clarion-8245039f6b`), subsystem clustering instability
(`clarion-14398b2536`) — none of which Phase 3 fixes.

## Cost evidence (bounded-additivity + weight)

1. **Bounded additivity is expensive (ADR-049 §2, finding H4).** rust-analyzer
   is admissible *only* if it reproduces the §1 qualname **byte-for-byte**; any
   newly-revealed (e.g. macro-expanded) entity goes through "this same locator +
   SEI + parity-fixture contract — it is *not* free additivity." The qualname
   dialect is already on its **fourth amendment** and is co-owned with Wardline
   as a second producer, pinned by a shared conformance corpus. A second
   in-tree resolution engine that diverges by one byte **forks every entity id**
   (SEI locator churn, Wardline taint mis-key, Filigree association drift). The
   gate is the corpus; clearing it for a fundamentally different frontend
   (semantic, macro-expanding) is a large, ongoing conformance burden.

2. **Weight violates the local-first posture.** rust-analyzer needs `cargo
   metadata` (registry/index resolution = **network egress**) and a buildable
   project (**toolchain**) — ADR-049 §2 calls this "the binding exclusion" from
   the `analyze` path. CLAUDE.md: "No mandatory cloud component… `analyze` runs
   with no credentials." Sprint-3 already showed `syn` analyze is cheap (tokio
   11 s / 43 MiB; rust-analyzer corpus 142 s / 121 MiB); a rust-analyzer pass
   would multiply RAM/index cost and drag a heavy dep into a tool that prides
   itself on having none.

## Options

- **Build now.** Pay the byte-for-byte conformance + heavy-dep cost immediately
  to convert the large unresolved-call remainder into resolved edges. Rejected
  fit: spends the scarce qualname-conformance budget on enrichment while
  higher-severity *correctness* defects (silent collisions, file drops) are
  still open, and breaches local-first by default.
- **Defer behind a demand threshold (recommended).** Keep the syn MVP; treat
  Phase 3 as gated, opt-in, additive-only. Pre-commit to a measurable trigger —
  e.g. a ratified consumer (Wardline taint or an MCP path) demonstrating that
  unresolved call sites *block* a real query, AND a design that keeps it
  off-by-default behind an explicit "allow heavy/networked backend" flag, AND a
  conformance plan that re-uses the existing corpus gate.
- **Drop.** Remove Phase 3 from scope; declare the syn MVP the permanent
  resolution ceiling. Rejected: the unresolved remainder is genuine, and ADR-049
  §2 already records rust-analyzer as the sanctioned future path — dropping
  discards a decided option for a demand that exists but is not yet *binding*.

## Recommendation

> **DEFER** — keep the honest `syn` MVP, hold rust-analyzer enrichment behind a
> ratified, measurable demand threshold and an off-by-default heavy/networked
> backend flag; do not build it until a real consumer is blocked by unresolved
> call sites and the correctness-defect queue (collisions, file drops) is clear.

## Tickets to file if accepted

1. **Phase-3 demand-threshold tracker** (P3, deferred): records the trigger
   condition (ratified consumer blocked by unresolved call sites) and the
   off-by-default heavy-backend flag requirement; depends on the correctness
   queue (`clarion-dfeb905f46`, `clarion-8245039f6b`) closing first.
2. **Unresolved-call-rate metric** (P3): emit per-corpus resolved-vs-unresolved
   `calls` counts from `analyze` (today only harvested ad hoc in `/tmp`) so the
   demand threshold is measured, not estimated.
3. **ADR-049 §2 annotation** (P4, docs): note that Phase 3 remains DEFERRED per
   this memo, with the demand threshold as the entry condition — so the "admitted
   later" clause is not read as a standing go.
