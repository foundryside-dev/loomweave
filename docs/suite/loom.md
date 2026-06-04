# Loom

**Audience**: anyone designing, extending, or evaluating whether a new product belongs in the Loom family
**Purpose**: establishes the strategic direction, composition law, and go/no-go test that govern Loom as a suite
**Companion**: [briefing.md](./briefing.md) for an introductory 5-minute read

---

## 1. What Loom is

Loom is a suite for enterprise-grade code governance on small teams. Its first tools are **Clarion**, **Filigree**, and **Wardline**, each fully authoritative in its domain and fully usable on its own. When composed, they enrich one another through narrow, additive protocols — but each remains independently load-bearing for the work it already does. A fourth product, **Shuttle**, is proposed and not yet in flight.

The metaphor is deliberate: distinct threads stay distinct but gain value by being woven together. Loom is a **family name** and a **composition doctrine** — not a platform, not a shared runtime, not a store, and not a broker. There is nothing called "Loom" to install, deploy, or keep running. What exists are the member products, and a set of narrow interop contracts between them.

## 2. The products and their authoritative domains

Each Loom product is authoritative for exactly one bounded concern, and that authority lives in the product itself — not in any shared layer:

- **Clarion** — structural truth about the codebase. Answers "what is this codebase and where should I touch?" Owns the entity catalog, the code graph, and guidance sheets.
- **Filigree** — work state and workflow lifecycle. Answers "what work exists, what state is it in, and what happened?" Owns issues, observations, and finding triage state.
- **Wardline** — trust policy and rule enforcement. Answers "what is allowed, and does this still satisfy the declared constraints?" Owns trust declarations, baselines, and policy findings. Notably, Wardline's "configuration" is the source code itself plus the adjacent declarations — it does not have a separate authoritative config store.
- **Shuttle** *(proposed)* — transactional scoped change execution. Answers "carry this approved change through the weave, under guard rails." Would own the execution record of applied changes and their rollback provenance.

Shuttle's scope is deliberately narrow: it receives a scoped change intent, binds it to files or entities, orders the edits, applies them incrementally with pre- and post-change checks, rolls back on failure, and lints / commits / emits telemetry on success. It does not plan, triage, or reason about the code it is editing.

## 3. Federation, not monolith

**Loom is a federation, not a monolith. Each member product is authoritative in one bounded domain. Integration must be additive, not compulsory. No Loom product may require the full suite to justify its existence.**

This is the founding architectural law. There is no Loom runtime, no Loom config layer, and no Loom store. Loom is a family name, a composition doctrine, and a set of narrow interop contracts — nothing more. The rule protects against the stealth-monolith failure mode: a "lightweight glue layer" that quietly becomes the real system of record, reducing sibling products to thin clients and making solo mode dishonest.

## 4. The composition law

Any Loom product must satisfy all three modes:

- **Solo mode** — the product has a complete, respectable use-case by itself
- **Pair mode** — combined with any one sibling, it creates a meaningful capability, not a broken fragment
- **Suite mode** — all together form something richer, but suite mode must never be mandatory for basic usefulness

Pairwise composability is a hard rule, not an aspiration. A product that only works when all siblings are present is a feature of a monolith wearing modular clothing.

## 5. Enrichment, not load-bearing

**A sibling product may enrich another product's view, but it must never be required for that product's semantics to make sense.**

This is the rule that keeps integration additive. It has a concrete test and concrete consequences:

### The failure test

The principle has three failure modes. Any one of them means Loom has centralised too far:

1. **Semantic coupling** — if removing a sibling product changes the *meaning* of another product's own data. Sibling absence may reduce convenience or automation; it must not alter semantics. Less capability is acceptable; incoherent data is not.
2. **Initialization coupling** — if a product cannot start, self-test, or validate its own configuration without a sibling being present. The product may degrade its capabilities in the sibling's absence; it must not fail to boot.
3. **Pipeline coupling** — if a pair of sibling products (X, Z) cannot exchange data except through a third sibling (Y). Each pair's ability to compose must be independent of any uninvolved third product — pairwise composability (§4) is not satisfied if the pair silently routes through an absent mediator.

A "standalone mode" that works only because an invisible sibling is still imported, or a "pairwise mode" that actually routes through an absent mediator, is not federation.

### Concrete examples

- **Filigree** creates and closes tickets exactly the same way whether Clarion is installed or not. Clarion makes the tickets richer — entity context, file references, structural findings linked to issues — but doesn't change their meaning. You can file a bug, work it, and close it with Clarion absent or broken.
- **Wardline** enforces trust policy whether Filigree is ingesting findings or not. Findings reach Wardline's own SARIF output regardless of whether a downstream triage system exists.
- **Clarion** builds its catalog whether Wardline is present or not. Wardline's annotations *enrich* Clarion's entity metadata with trust-tier and policy-semantic information, but Clarion's structural truth is independent of Wardline's policy truth.
- **Shuttle**, if built, would execute changes whether any sibling is present. Sibling tools enrich its telemetry (which Filigree ticket? which Clarion entity? which Wardline policy?) but are never required for a change to apply or roll back.

### v1.0 asterisks

The v1.0 suite does not pass the expanded failure test cleanly. Two specific couplings are named here so they cannot drift unnoticed:

- **Wardline→Filigree findings are pipeline-coupled through Clarion in v1.0.** Wardline's SARIF output reaches Filigree only via Clarion's `clarion sarif import` translator. This violates pipeline composability for the (Wardline, Filigree) pair. *Retirement condition* (mechanism updated 2026-05-29): the generic Wardline rebuild ships a **native Filigree emitter** (Wardline-side, per the 2026-05-29 integration brief and Clarion's ADR-015 Revision 2), at which point the pair composes directly and Clarion drops off the transport path — its `clarion sarif import` translator stays as the general-purpose SARIF path for other tools; only its Wardline-bridge role retires. The asterisk is **kept live** until that emitter ships and (Wardline, Filigree) composition is verified with Clarion absent — agreement to the direction is not retirement. Tracked under `release:1.1`.
- **Retired 2026-06-05 — Clarion's Python plugin imported `wardline.core.registry.REGISTRY` at startup.** This was initialization coupling scoped to the Wardline-aware plugin specifically, not to Clarion as a product. The retirement condition was met in two parts: Wardline shipped the NG-25 trust-vocabulary descriptor, and Clarion's Python plugin now reads that descriptor (`.wardline/vocabulary.yaml` first, then the installed `wardline/core/vocabulary.yaml` data file) without importing `wardline`, `wardline.core`, or `wardline.core.registry`. Clarion records only source-observed decorator metadata on its own entities; Wardline remains authoritative for vocabulary and policy semantics. See ADR-018 Revision 3. The asterisk is no longer live on the Clarion side.

Asterisks are acceptable only with a written retirement condition and an honest statement of which failure-test mode is being temporarily violated. A "we'll fix it later" without a test-mode citation is not an asterisk; it is the stealth-monolith failure mode wearing different clothes.

### Why this matters

Enrichment is the shape of integration that preserves federation. Load-bearing integration collapses federation into monolith by another name. The moment one product *needs* another to make sense of its own data, the composition law becomes dishonest — "standalone mode" works only because the sibling is still running somewhere, and the illusion of modularity collapses the first time deployment doesn't match.

## 6. What Loom is NOT

Because the strongest pressure on this charter comes from "wouldn't it be easier if we just…" proposals, the disclaimer is explicit. Loom is **not**:

- **A shared runtime or daemon.** There is no `loomd`, no broker, no orchestrator. Member products do not phone home to a Loom process.
- **A shared configuration layer.** Each product configures its own integrations in its own config. Clarion's config names Filigree's endpoint directly; there is no central registry that everyone consults.
- **A central store or database.** Each product owns its data locally. No shared SQLite/Postgres/object-store sits under the suite.
- **A system of record for any cross-product state.** Finding lifecycle lives in Filigree. Entity identity lives in Clarion. Policy baselines live in Wardline. Execution provenance (if Shuttle ships) lives in Shuttle. Loom does not own or mirror these.
- **An identity reconciliation service.** When cross-scheme translation is needed — e.g. Wardline qualname → Clarion entity ID — the product that *cares* does the translation, because that product is the one whose authority needs it. Clarion translates qualnames because Clarion owns the catalog that makes them meaningful. There is no neutral "Loom identity oracle."
- **A capability negotiation bus.** Products probe each other directly via
  their own surfaces (HTTP endpoints, MCP tools, CLI flags). Version skew is
  handled bilaterally, not through a Loom-level registry. Clarion's HTTP read
  API is one such bilateral surface; its operator trust model is documented in
  [`docs/operator/clarion-http-read-api.md`](../operator/clarion-http-read-api.md):
  the HTTP surface is unauthenticated, loopback-only by default, and requires an
  authenticated reverse proxy or equivalent control before any non-loopback
  bind.

The test for any proposed addition: if the proposal introduces something that would need to be *running* or *present* for the suite to work, it violates federation. Integration protocols, schemas, and narrow contracts are fine. Shared infrastructure that sibling products *depend on* is not.

## 7. The go/no-go test for future products

Before adopting any new product into Loom, it must pass all four:

1. **Is it authoritative for one narrowly bounded thing?** — if the scope is two or more things, it is two or more products.
2. **Is it useful by itself?** — if siblings are required for minimum utility, it is a feature or adapter, not a product.
3. **Does it form a sensible story with each existing product one-to-one?** — every pairing must yield a coherent workflow; no "this only matters when you also have X and Y" patterns.
4. **Is the full suite better because of it, without making the others incomplete in its absence?** — addition, not patching.

If the answer to any question is no, the candidate is a feature, a protocol, or an adapter — not a product. It may still belong in Loom's surface area, but not as a named member.

## 8. Naming

Member products are named from weaving mechanics — Clarion, Filigree, Wardline, Shuttle — as distinct proper names rather than subdivisions. There is no "Loom Guard," "Loom Workflow," or "Loom Execute"; each product earns its own identity. The family name sits above the products without dominating them, and — per §3 and §6 — it does not name any component that gets installed or runs.

### Cross-product field names

Federation does not require uniform vocabulary, but it does require that the same word never silently means two things across siblings. The discipline:

- A single word should mean the same thing across products, OR be confined to one product, OR have an explicit mapping documented in an Accepted ADR.
- The suite-level catalogue of cross-product-visible terms lives in [`glossary.md`](./glossary.md). It is a read-only design-review artefact (per §5: nothing imports it, nothing runs from it, removing it changes no semantics) and is federation-safe by construction.
- Each product enforces the rule locally in its own ADR-acceptance process (Clarion's lives in [`docs/clarion/adr/README.md`](../clarion/adr/README.md)). Other products are expected to mirror the rule in their own ADR processes, citing this section as the suite-level authority. CI lint or cross-repo enforcement is explicitly out of bounds — that would convert a federation-safe doc into shared infrastructure.

## 9. Status

| Product | Status |
|---|---|
| Clarion | Built; v1.0 shipped against the published design (walking skeleton was tagged `v0.1-sprint-1`); first-customer target is elspeth (~425k LOC Python) |
| Filigree | Built; in active use |
| Wardline | Built; in active commit-cadence use |
| Shuttle | Proposed; not in flight; separate design effort when prioritised |

The v0.1 Loom suite is Clarion + Filigree + Wardline. Shuttle enters the suite only when it passes the go/no-go test (§7) and has its own spec, design, and validating customer.

This charter is expected to outlive v0.1 and shape all subsequent product gates. Its load-bearing sentence is in §5: **enrichment, not load-bearing**. If that principle is ever compromised, the rest collapses.
