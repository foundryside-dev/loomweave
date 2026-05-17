# ADR-029: Entity Associations — Filigree Binding for Clarion Entities

**Status**: Accepted
**Date**: 2026-05-16
**Deciders**: qacona@gmail.com
**Context**: Sprint 2's mid-sprint scope amendment (`docs/implementation/sprint-2/scope-amendment-2026-05.md`) names "an issue tracker that knows what code an issue is about" as the day-one value the MVP MCP surface delivers. Filigree already has `file_associations` (issue↔file relationships) but no concept of an issue being about a Clarion *entity* (function / class / module). WP9 in `v0.1-plan.md` scopes Loom integrations around findings emission (Clarion `findings.jsonl` → Filigree `/api/v1/scan-results`). Entity associations are a different concern that the WP9 scope did not name. This ADR (a) defines the binding shape, (b) splits WP9 into A (entity binding, v0.1) and B (findings emission, v0.1 or v0.2), (c) argues federation §5 compliance, and (d) specifies the content-hash drift detection that makes the binding survive code edits.

## Summary

Three decisions:

1. **Filigree owns the binding table.** A new `entity_associations(issue_id, clarion_entity_id, content_hash_at_attach, attached_at, attached_by)` table lives in Filigree, not Clarion. Clarion entity IDs (per ADR-003 — `{plugin_id}:{kind}:{canonical_qualified_name}`) are stored as opaque strings; Filigree does not parse them or know what a "Clarion entity" is semantically. The federation §5 enrich-only rule passes (argued in §"Federation check" below).
2. **Two MCP tools front the binding.** `add_entity_association(issue_id, entity_id)` on Filigree's MCP server attaches an entity to an issue (and snapshots the current content_hash). `issues_for(entity_id, include_contained: bool = true)` on Clarion's MCP server queries Filigree's HTTP API and returns the issues attached to the entity (and, transitively, anything contained beneath it).
3. **Drift detection via content_hash snapshot at attach time.** When an association is created, Filigree stores Clarion's current `entities.content_hash` for that entity. Clarion's `issues_for` query compares the snapshotted hash to the current hash; mismatches are returned in a `drifted: [...]` envelope alongside the matched issues. Drifted associations are not automatically broken — the consult-mode agent decides whether the issue still applies. The MCP tool surfaces the drift; Filigree does not unilaterally invalidate.

## Context

### Why a new table, not `file_associations`

`file_associations` already exists on the Filigree side: it links an issue to a path string. Entity associations are not a generalization of file associations — they're a peer concept with different semantics:

- **Granularity.** A file association anchors to a path, which is stable across edits within the file. An entity association anchors to a function or class, which can move within the file (refactored, split, renamed) without the file path changing.
- **Identity.** A file's identity is its path. An entity's identity is `{plugin_id}:{kind}:{canonical_qualified_name}` — ADR-003's three-segment composite. Conflating them in one table loses the distinction.
- **Drift semantics.** A file's contents changing doesn't break the file association (the file still exists at the path). An entity's qualname changing (rename, move) breaks the entity association — the old ID no longer resolves to a current entity. Content-hash drift is the warning signal; qualname drift is a hard break (handled by `issues_for` returning `not_found` for the old ID).

Both kinds of association coexist. An issue can be associated with a file AND with one or more entities inside that file. The two are read independently.

### Why Filigree owns the table, not Clarion

The candidate decision is "who is the durable store for this fact" — Filigree or Clarion. Filigree wins on three counts:

- **Lifecycle alignment.** An issue outlives any single Clarion scan. Clarion is re-scannable from source; Filigree is not (issues, comments, history are durable). Storing entity associations on the Clarion side would mean a `clarion install` in a fresh checkout has no associations until the next scan-and-attach pass, which contradicts "I cloned the repo and want to ask which issues are about this function."
- **Existing pattern.** Filigree already stores `file_associations`, `dependencies`, `comments`, `labels` — all the issue-side metadata layers. `entity_associations` is the same shape of decision: an issue-side fact about what the issue is about.
- **Query direction.** The two relevant queries are "given an issue, what entity is it about?" (Filigree-side: cheap join from `issues`) and "given an entity, what issues are attached?" (Clarion calls Filigree's HTTP API). Both are served better when the durable store is on the Filigree side.

### Federation check (loom.md §5 — enrich-only rule)

The §5 failure modes:

1. **Semantic coupling** — does Filigree depend on Clarion to function?
   No. The `entity_associations` table stores opaque string IDs. Filigree does not parse them, validate them, or know what a "Clarion entity" is. An installation of Filigree without Clarion sees empty `entity_associations` and operates normally. Issue create / update / close / dependency / label / comment all work without Clarion.

2. **Initialization coupling** — must Filigree wait for Clarion to start, or vice versa?
   No. The binding is created and queried only when both products are present. Clarion startup does not check for Filigree (the existing `--no-filigree` flag short-circuits the integration cleanly). Filigree startup does not check for Clarion.

3. **Pipeline coupling** — does Filigree's data become wrong if Clarion goes away?
   No. An issue with an entity association is a complete, semantically-valid Filigree issue with extra metadata. The association becomes "unresolvable" (Clarion can't tell you what entity the string refers to), but the issue itself is intact.

The binding enriches both sides without making either depend on the other for core semantics. This is the federation axiom satisfied.

### Why split WP9 into A and B

The original WP9 in `v0.1-plan.md` bundles two distinct integration stories:

- **WP9-A** (this ADR): entity_associations binding. Issue ↔ entity. New durable Filigree state. Surfaces issues_for() on Clarion's MCP for the "where am I and what's outstanding here" agent question.
- **WP9-B** (deferred): findings emission. Clarion's `findings.jsonl` → Filigree's `/api/v1/scan-results`. Adds Clarion as a finding source alongside Wardline and other scanners.

WP9-A is on the MVP critical path because `issues_for` is one of the seven MCP tools. WP9-B is valuable but not MVP — Clarion can produce a `findings.jsonl` artifact locally without POSTing it; agents can ask the human to import it later. The split lets WP9-A land in v0.1 without dragging WP9-B's full ADR-004/017/018 reconciliation along.

The two halves are independent: WP9-A introduces `entity_associations`, `add_entity_association`, `issues_for`. WP9-B introduces `scan_source` field on findings, dedup policy, severity round-trip. No cross-cutting refactor; no flag-day.

## Decision

### Decision 1 — Filigree-side `entity_associations` table

The new Filigree migration introduces:

```sql
CREATE TABLE entity_associations (
    issue_id                TEXT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    clarion_entity_id       TEXT NOT NULL,
    content_hash_at_attach  TEXT NOT NULL,
    attached_at             TEXT NOT NULL,  -- ISO 8601
    attached_by             TEXT NOT NULL,  -- actor identity
    PRIMARY KEY (issue_id, clarion_entity_id)
);

CREATE INDEX ix_entity_assoc_entity ON entity_associations(clarion_entity_id);
```

Notes:

- `clarion_entity_id` is opaque to Filigree. No `CHECK` constraint validates its grammar (validation would couple Filigree to ADR-003's segment format; the federation axiom forbids it). Malformed IDs are an MCP-tool-side concern.
- `content_hash_at_attach` is a snapshot of Clarion's `entities.content_hash` at the moment of attachment. Filigree does not interpret it — it's a blob Filigree hands back to Clarion at query time so Clarion can compare against the current state.
- `ix_entity_assoc_entity` lets `issues_for` resolve in O(log N) per entity_id rather than scanning. The primary key already covers the issue-side direction.
- `ON DELETE CASCADE` on `issue_id` ensures association rows die with the issue. There is no cascade on the entity side (Filigree cannot detect entity deletion; that's Clarion's domain).

### Decision 2 — Two MCP tools front the binding

**On Filigree's MCP server** (new tool):

```
add_entity_association(issue_id: str, entity_id: str, content_hash: str) -> AssociationResult

  Attaches a Clarion entity to a Filigree issue. The content_hash argument is
  the entity's current content_hash, snapshotted at attach time for later
  drift detection. The caller (typically Clarion's MCP server proxying for a
  consult-mode agent, or a human operator) is responsible for fetching the
  current content_hash from Clarion before calling.

  Returns the created or updated association row; the operation is idempotent
  on (issue_id, entity_id) — re-attaching updates content_hash_at_attach and
  attached_at, preserving the original attached_by.
```

A peer `remove_entity_association(issue_id, entity_id)` tool removes the binding. A peer `list_entity_associations(issue_id)` tool enumerates associations for an issue (used by `issues_for`'s inverse direction and by issue-detail UI).

**On Clarion's MCP server** (new tool):

```
issues_for(entity_id: str, include_contained: bool = true) -> IssuesForResult {
    matched:  [{ issue_id, association_attached_at, drift_status, ... }],
    drifted:  [{ issue_id, content_hash_at_attach, current_content_hash, ... }],
    not_found: [issue_id_referencing_missing_entity, ...],
}

  Returns the Filigree issues attached to entity_id. If include_contained
  is true (default), also returns issues attached to any entity transitively
  reachable through contains edges (so an issue attached to a class shows
  up when asking about one of its methods).

  Clarion performs the HTTP call to Filigree (using the existing
  client/auth from --no-filigree's enable path), fetches the association
  rows, then for each row computes drift_status by comparing the
  stored content_hash_at_attach to the current entities.content_hash.
```

The Clarion-side tool is the integration surface for consult-mode agents. The Filigree-side tools are the durable-state surface for write operations. The two MCP servers do not share state — they communicate only via Filigree's existing HTTP API.

### Decision 3 — Drift detection via snapshot, not invalidation

When an association is created, the content_hash of the entity at that moment is stored alongside the association. Three states emerge from a `issues_for` query:

- **matched** — the entity exists; current `content_hash == content_hash_at_attach`. The association is fresh.
- **drifted** — the entity exists; current `content_hash != content_hash_at_attach`. The code under the entity changed since the association was created; the issue's anchor moved. Returned in the `drifted` envelope so the consult-mode agent can decide.
- **not_found** — the entity_id does not resolve to any current entity. The qualname changed (rename, refactor, deletion). Returned in `not_found`; Filigree's row is preserved so a human can re-anchor or close the issue.

**Why snapshot-and-flag, not invalidate-on-write.** Invalidating an association the moment code changes would create thrash: every commit to a frequently-edited file would orphan its associations. Snapshotting the hash at attach time and surfacing drift at query time lets the agent decide whether the change is material (the issue's still valid, just on slightly newer code) or invalidating (the issue's about a function that's been completely rewritten). The flag is information; the action is the agent's.

**Why hash snapshots, not version snapshots.** Clarion's `entities.content_hash` is already computed and indexed (`detailed-design.md` §3). Snapshotting it is a string copy. The alternative — snapshotting Clarion's analyze run_id — would require Filigree to know about Clarion's run lifecycle, which violates the federation axiom.

## Alternatives Considered

### Alternative 1 — Clarion owns the binding table

`entity_associations` lives in `.clarion/clarion.db`; Filigree calls Clarion's HTTP read API to enumerate associations per issue.

**Why rejected**: lifecycle inversion. Issues are durable; `.clarion/` is reproducible from source plus a re-scan. Storing the binding on Clarion's side means associations vanish when `.clarion/` is rebuilt (fresh checkout, schema migration, deliberate reset). The "fresh-checkout-and-ask-issues_for" use case becomes broken-by-design.

### Alternative 2 — Generalize `file_associations` to support entity IDs

Add an `association_type` column to `file_associations` distinguishing 'file' from 'entity'; entity_id rides in the existing path column.

**Why rejected**: overloading. The existing `file_associations` consumers (Filigree-native scanners, file-context displays) would all need to filter by `association_type`, and the drift-detection mechanism (content_hash snapshot) is meaningless for the 'file' kind. Two tables with different semantics are clearer than one table with a discriminator the readers must handle.

### Alternative 3 — Shared database between Clarion and Filigree

Skip the HTTP indirection; both products read from a shared SQLite DB.

**Why rejected**: explicit federation violation (loom.md §5). Shared schema couples upgrade cycles; one product's schema migration becomes the other product's deployment blocker. The two-database, HTTP-mediated design is the federation axiom in action.

### Alternative 4 — Eager drift propagation: Clarion notifies Filigree when entities change

Clarion's `analyze` posts a delta to Filigree's MCP after each run; Filigree updates a `drift_status` column on `entity_associations`.

**Why rejected**: introduces pipeline coupling. Filigree starts requiring Clarion to keep its drift_status field current. The lazy "snapshot-at-attach + compare-at-query" path moves the same information without coupling the write path.

### Alternative 5 — Two-way binding: Clarion stores a list of (entity → issue_ids) too

For query performance.

**Why rejected**: cache invalidation. Two writers to the same logical fact. The `issues_for` query path goes through Filigree's HTTP API once per query, which is acceptable at MCP query rates (one-shot agent question, not high-frequency). If query latency becomes a bottleneck, a TTL'd Clarion-side cache is the right intervention — not a second source of truth.

## Consequences

### Positive

- **Day-one value.** "What issues are about this code I'm reading?" is the question the consult-mode agent most often needs to ask. `issues_for` makes it one MCP call.
- **Federation clean.** Both products work independently; both work better together. §5 audit passes per-failure-mode.
- **Drift is honest.** The agent sees that code under an issue changed without the association silently breaking. This is the same epistemic discipline as ADR-028's confidence tiers — surface uncertainty, don't paper over it.
- **WP9 unblocked at the MVP-relevant half.** WP9-A ships entity binding in Sprint 2 (resumed); WP9-B (findings emission) can stay scheduled in v0.1 or slip to v0.2 without holding up MCP surface delivery.

### Negative

- **Two MCP servers in play for one workflow.** The agent calls `add_entity_association` on Filigree's MCP (write path) and `issues_for` on Clarion's MCP (read path). The agent harness needs both MCP servers configured. Filigree's MCP is already a thing (per CLAUDE.md instruction); Clarion's MCP is WP8 (B.6 in the resumed sprint). No new infrastructure, but the configuration surface widens.
- **Cross-product schema migration.** Filigree gets a new migration. The Filigree codebase is at v2.0.1 on its own release rhythm; the migration lands as part of a Filigree minor release (target: 2.1.0). Coordination cost is one PR on the Filigree side, sequenced before Clarion's MCP server can call `add_entity_association`.
- **`include_contained` semantics need design care.** "Issues attached to anything contained beneath this entity" assumes a clean contains-graph traversal. For module → class → method depths of 3-4, this is cheap. For pathological cases (a huge module with thousands of contained entities, each with attached issues), `include_contained` could return a long list. The MCP tool should paginate; the v0.1 implementation can cap at a reasonable size (e.g., 100 issues) with a `truncated: bool` flag.
- **Reverse-lookup index size.** `ix_entity_assoc_entity` on Filigree grows with the number of associations across all issues. At expected v0.1 scale (hundreds of associations across the elspeth scale-test corpus), negligible. At enterprise scale (millions of associations), partition-by-prefix or similar. Out of scope for v0.1.

### Neutral

- The `attached_by` actor field uses Filigree's existing actor-identity mechanism (`--actor` flag, MCP session identity). No new identity surface.
- The HTTP call from Clarion → Filigree uses the existing Filigree HTTP API; the `--no-filigree` flag (per WP9 scope in `v0.1-plan.md`) short-circuits the `issues_for` call, returning an empty result with a `filigree_unreachable: true` flag.

## Open Questions

- **Filigree release sequencing.** Does the `entity_associations` migration land in Filigree 2.0.2 (patch), 2.1.0 (minor), or wait for 2.1.x? Filigree's current branch is `release/2.0.1`. Recommendation: 2.1.0, paired with the new MCP tool registrations. Final call belongs to the Filigree-side release plan, not this ADR.
- **Authentication for `add_entity_association`.** Does Filigree's MCP enforce write-side auth? Existing MCP tools (e.g., `create_issue`) use the same actor identity; this ADR assumes that pattern. To be confirmed in the Filigree-side implementation.

## Related Decisions

- [ADR-003](./ADR-003-entity-id-scheme.md) — entity ID grammar. Filigree treats the ID as opaque; the grammar is Clarion's contract with itself, surfaced through `issues_for`.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — Filigree's pluggable `RegistryProtocol`. The `entity_associations` table is a peer concept, not a use of `RegistryProtocol`. The two can coexist; the `clarion` backend mode in ADR-014 is a separate (still-scheduled) v0.1 deliverable.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — core/plugin ontology boundary. Filigree is a peer product, not a Clarion plugin; the ontology rules do not apply to the Filigree-Clarion link.
- [`loom.md` §5](../../suite/loom.md) — federation failure modes. ADR-029 cites the three rules and argues compliance per-mode; failed audits would block this ADR.
- [WP9 in `v0.1-plan.md`](../../implementation/v0.1-plan.md#wp9--loom-integrations-filigree--wardline-clarion-side) — the work package this ADR splits in two.

## References

- [Sprint 2 scope-amendment memo](../../implementation/sprint-2/scope-amendment-2026-05.md) — the larger context: MVP MCP surface, entity_associations as the day-one value, sequencing.
- [Filigree CLAUDE.md (project-root)](/home/john/CLAUDE.md) — existing MCP tool inventory; this ADR adds `add_entity_association`, `remove_entity_association`, `list_entity_associations`.
- [Filigree schema discovery — `GET /api/files/_schema`](https://github.com/tachyon-beep/filigree) — current file-side endpoints; entity-side endpoints are net-new.
