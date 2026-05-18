# ADR-031: Schema-Validation Policy for Enum-Shaped TEXT Columns

**Status**: Accepted
**Date**: 2026-05-18
**Deciders**: qacona@gmail.com
**Context**: F-13 from the 2026-05-03 skeleton audit observed that the
`0001_initial_schema.sql` migration carries six enum-shaped `TEXT` columns
without `CHECK` constraints, while two sibling columns in the same migration
do (`edges.confidence`, `summary_cache.stale_semantic`). The divergence is
unprincipled and undocumented. ADR-024 § Consequences/Neutral named F-13 as
out-of-scope for that ADR and deferred a policy decision here. Triggered by
filigree issue `clarion-fbe50aa6e1` (P2).

## Summary

Schema-level `CHECK` constraints are required on enum-shaped `TEXT` columns
whose vocabulary is **closed and core-owned**, and forbidden on columns whose
vocabulary is **plugin-extensible** (ADR-022). For v0.1 this means
`findings.{kind, severity, status}` and `runs.status` gain `CHECK` clauses
matching the values defined in ADR-004, ADR-017, and the `RunStatus` Rust
enum; `entities.kind` and `edges.kind` do **not** receive `CHECK` clauses
because ADR-022 reserves edge-kind and entity-kind vocabulary to the plugin
manifest. The writer-actor (per ADR-011) remains the canonical normal-path
validator — typed Rust enums at the command boundary (`commands.rs`) and
`enforce_edge_contract` (`writer.rs:411`) prevent bad values from reaching
the SQL layer. `CHECK` constraints are defense-in-depth: they catch
hand-typed string literals in writer-actor code (e.g., `'running'` at
`writer.rs:322`), accidental drift when a Rust enum gains a variant without
a schema update, and any future debugging path that bypasses the writer
(`sqlite3` CLI, repair scripts). The two-layer model is the same shape as
`edges.confidence` already uses; ADR-031 generalises that precedent to a
documented policy. Migration `0001_initial_schema.sql` is edited in place
under the ADR-024 in-place edit policy (no external consumers exist).

## Context

The 2026-05-03 skeleton-audit reviewer additions flagged F-13:

> Enum-typed `TEXT` columns lack `CHECK` constraints (`entities.kind`,
> `edges.kind`, `findings.{kind,severity,status}`, `runs.status`);
> validation lives in the writer-actor by design but the policy is
> undocumented.

The audit deferred a fix to its own ADR. The motivating bug —
`clarion-4cd11905e2`, closed by ADR-024 — was the priority-affinity slip
where a TEXT-affinity column silently accepted any string the writer
inserted. The audit's framing was that one closed bug was the *first
instance* of a class; every enum-shaped TEXT column carries the same latent
defect until the policy is named.

Two in-house precedents already use `CHECK`:

- `edges.confidence` (`0001_initial_schema.sql:80-81`) — `CHECK (confidence
  IN ('resolved', 'ambiguous', 'inferred'))`, per ADR-028.
- `summary_cache.stale_semantic` (`0001_initial_schema.sql:134`) — `CHECK
  (stale_semantic IN (0, 1))`.

Both columns have closed, core-owned vocabularies. Both are defended by
typed Rust types (`EdgeConfidence`, `bool`) at the writer boundary. The
existence of the CHECK on `edges.confidence` two lines above `edges.kind`
(which has no CHECK) is the visual evidence that the project's *intended*
policy is defense-in-depth — the policy was just never written down, so the
treatment is inconsistent.

The columns flagged by F-13 split into two categories:

1. **Closed, core-owned vocabulary** (CHECK applies):
   - `findings.kind` — `defect | fact | classification | metric | suggestion`
     (5 values, ADR-004 + `detailed-design.md:275`).
   - `findings.severity` — `INFO | WARN | ERROR | CRITICAL | NONE`
     (5 values, ADR-017).
   - `findings.status` — `open | acknowledged | suppressed | promoted_to_issue`
     (4 values, `detailed-design.md:295`).
   - `runs.status` — `running | skipped_no_plugins | completed | failed`
     (4 values; the three terminal states are the `RunStatus` enum in
     `commands.rs:26`; `'running'` is the in-flight literal inserted at
     `writer.rs:322` during `BeginRun`).

2. **Plugin-extensible vocabulary** (CHECK does not apply):
   - `entities.kind` — plugin-declared per ADR-022 §"Plugin owns
     (ontology-vocabulary)". Core enforces only the identifier grammar
     `[a-z][a-z0-9_]*` and the three reserved-kind names; the universe of
     valid kinds is unbounded.
   - `edges.kind` — also plugin-declared per ADR-022. The current writer
     (`writer.rs:394-401`) hardcodes a 9-value ontology for the v0.1
     Python-plugin shape, but ADR-022 §"Plugin owns" explicitly admits
     plugin-declared edge kinds beyond the four core-reserved structural
     ones. A schema-level CHECK would over-constrain the data layer
     against ADR-022's contract.

The non-flagged columns (e.g., `edges.confidence`, `summary_cache
.stale_semantic`) are already CHECK-protected and remain so.

`findings` is not yet written by any Sprint-1 or Sprint-2 code — the table
is forward-defined for WP4+. The CHECK additions to `findings` are
zero-blast-radius today; they become the schema invariant the first WP4 PR
will encode against.

## Decision

### Policy

A migration must include a `CHECK (column IN (...))` constraint on every
`TEXT` column whose value set is **closed and core-owned**. A migration
must **not** include a `CHECK` constraint on a `TEXT` column whose value
set is plugin-declared (ADR-022). Numeric enum-shaped columns (e.g.,
`summary_cache.stale_semantic`) follow the same rule: closed → CHECK,
extensible → no CHECK.

"Closed and core-owned" means: the complete value set is enumerated in an
Accepted ADR or in a core-owned Rust type (`enum` plus, if applicable,
the small set of string literals the writer uses for in-flight states).
Adding or renaming a value requires both an ADR (or ADR amendment) and
a paired migration update.

"Plugin-extensible" means: a plugin's startup manifest contributes
values per ADR-022 (§"Plugin owns"), and the universe of valid values is
not knowable at schema-creation time.

### v0.1 application

Migration `0001_initial_schema.sql` is edited in place under the
ADR-024 in-place edit policy (retirement trigger remains: first
external operator pulling a published Clarion build). The following
CHECK clauses are added:

| Column | CHECK clause | Source of truth |
|---|---|---|
| `findings.kind` | `CHECK (kind IN ('defect', 'fact', 'classification', 'metric', 'suggestion'))` | [ADR-004](./ADR-004-finding-exchange-format.md), `detailed-design.md:275` |
| `findings.severity` | `CHECK (severity IN ('INFO', 'WARN', 'ERROR', 'CRITICAL', 'NONE'))` | [ADR-017](./ADR-017-severity-and-dedup.md) |
| `findings.status` | `CHECK (status IN ('open', 'acknowledged', 'suppressed', 'promoted_to_issue'))` | `detailed-design.md:295` |
| `runs.status` | `CHECK (status IN ('running', 'skipped_no_plugins', 'completed', 'failed'))` | [ADR-011](./ADR-011-writer-actor-concurrency.md), `commands.rs:26` (`RunStatus`) + `writer.rs:322` (`'running'` literal) |

No CHECK is added to `entities.kind` or `edges.kind`. The schema comment
on each of those columns is updated to point at this ADR so a future
reader does not re-litigate the omission.

### Validation layers

The defense-in-depth model is two layers:

1. **Layer A — Writer-actor typed API (primary).** The writer-actor's
   command boundary (`commands.rs` `WriterCmd`, `RunStatus`,
   `EdgeConfidence`, the forthcoming `FindingKind`/`FindingSeverity`/
   `FindingStatus` enums when WP4 lands) constructs valid SQL values
   from typed Rust enums. `enforce_edge_contract` (`writer.rs:411`) is
   the additional per-kind contract layer for `edges`. This layer
   prevents bad values from being inserted in normal use; it produces
   structured `StorageError::WriterProtocol` errors with `CLA-INFRA-*`
   codes that surface in `runs.stats.failure_reason`.

2. **Layer B — SQL CHECK constraints (defense).** SQLite enforces the
   CHECK clauses on every write that reaches the SQL layer, regardless
   of which code path emitted it. Bad values produce
   `SQLITE_CONSTRAINT_CHECK` errors. This catches:
   - Hand-typed string literals in writer-actor code where a fat-finger
     bypasses the typed API (e.g., `INSERT ... VALUES (..., 'runing')`
     misspelling `'running'`).
   - Rust enum drift: a new `RunStatus` variant added without updating
     the CHECK list raises `SQLITE_CONSTRAINT_CHECK` at the first write,
     making the omission self-reporting at test time.
   - Out-of-band writes from `sqlite3` CLI debugging or future repair
     scripts.
   - Test-only paths that construct rows by raw SQL rather than through
     the writer.

### Test layer

The schema-apply test suite (`crates/clarion-storage/tests/schema_apply.rs`)
already asserts CHECK rejection on `edges.confidence` (test
`edges_confidence_column_rejects_unknown_tier` at line 538). This ADR adds
parallel rejection tests for each of the four newly CHECK-constrained
columns (`findings.{kind, severity, status}`, `runs.status`). The pattern
is: insert a row with a deliberately invalid value, assert the error
message contains `"CHECK constraint failed"`. Adding a new valid value
without updating both the CHECK clause and these tests will fail CI.

The writer-actor tests (`crates/clarion-storage/tests/writer_actor.rs`)
already exercise the Layer-A typed enums on the runs.status side
(`RunStatus::Completed`, `RunStatus::Failed`); no change required there.

### Schema-comment discipline

Each CHECK-constrained column gets a short SQL comment naming this ADR
and the source-of-truth ADR for its vocabulary:

```sql
status TEXT NOT NULL
       -- ADR-031: closed vocabulary; values from ADR-011 + RunStatus
       CHECK (status IN ('running', 'skipped_no_plugins', 'completed', 'failed')),
```

Each non-CHECK enum-shaped column (`entities.kind`, `edges.kind`) gets a
short SQL comment naming this ADR and the reason:

```sql
kind TEXT NOT NULL,
     -- ADR-031: plugin-extensible vocabulary (ADR-022); no CHECK by policy
```

The comments are the future-reader's first stop; without them the
non-uniform CHECK treatment looks like an oversight.

### Out of scope

This ADR does not address:

- The five remaining audit findings (F-14 through F-17 plus F-13
  itself) — each is a distinct schema concern with its own filigree
  issue (`clarion-ef9bd365bf`, `clarion-fb1b8fb5a0`,
  `clarion-523b2eebad`, `clarion-ba198ee96b`).
- JSON-shape validation on `properties`, `config`, `stats`, `evidence`,
  `related_entities`, `supports`, `supported_by` columns. These are
  TEXT-typed JSON blobs whose shape is owned by the writer-actor and
  validated (where validated at all) at the typed-Rust boundary. CHECK
  constraints on JSON shape are not a SQLite primitive and out of
  scope here; the policy is silent on them.
- `confidence_basis` and `suppression_reason` on `findings` — both are
  free-text fields with no enumerated vocabulary; no CHECK applies.

### When the policy switches

The policy switches from "edit `0001` in place" to "stack `0002_*.sql`"
at the same retirement trigger as ADR-024: the first time any external
operator pulls a published Clarion build and produces a
`.clarion/clarion.db`. After that point, vocabulary expansions for any
CHECK-constrained column require a stacked migration that rewrites the
table (SQLite cannot `ALTER TABLE ... DROP CHECK`; the canonical workaround
is the [SQLite "make other kinds of table schema changes" recipe](https://www.sqlite.org/lang_altertable.html#otheralter)
— rename, create new, copy, drop old, rename new — inside a single
transaction). Pre-trigger, the in-place edit is the lower-debt path.

## Alternatives Considered

### Alternative 1: No CHECK constraints anywhere; writer-actor is the only validator

Document explicitly that `CHECK` is not used; writer-actor is the sole
truth and tests assert writer-actor validation paths. Remove the existing
`edges.confidence` CHECK to make the policy uniform.

**Pros**: the simplest possible mental model — "validation lives in the
writer, full stop." No two-layer story to maintain. Schema migrations are
mechanical and never need a "what's the closed vocabulary today" check.

**Cons**: throws away free defense-in-depth. The `'running'` literal at
`writer.rs:322` is exactly the kind of typo a CHECK catches that the
writer-actor cannot — the typed `RunStatus` enum has no `Running` variant
(in-flight state is intentionally outside the terminal-status enum), so
the literal is hand-typed. A misspelling there would silently insert
`'runing'` into `runs.status`, and downstream queries filtering on
`status = 'running'` would silently miss the run. Tests would only catch
this if they happened to read back the status string. The CHECK closes
that hole at zero ongoing cost.

Removing the existing `edges.confidence` and `summary_cache.stale_semantic`
CHECKs would be a net loss in confidence with no compensating gain. The
audit's framing also pushes the other way: "validation policy undocumented"
is the bug; the answer "we don't validate at all" preserves the documented
fix but loses the existing protection.

**Why rejected**: removes working defense for ideological uniformity.

### Alternative 2: CHECK everything, including `entities.kind` and `edges.kind`, with a "registered kinds" mechanism

Add `CHECK` on `entities.kind` and `edges.kind` against a `registered_kinds`
lookup table populated at plugin-startup time. Manifest-declared kinds
land in `registered_kinds` before any insert; the CHECK becomes
`CHECK (kind IN (SELECT kind FROM registered_kinds))`.

**Pros**: uniform "every enum-shaped TEXT column has a CHECK" policy. No
exception for plugin-extensible vocabulary.

**Cons**: SQLite CHECK constraints cannot reference other tables (per
documentation — CHECK expressions are evaluated against the current row
only and may not contain subqueries). A foreign key would work, but a
foreign key on `kind` to `registered_kinds(kind)` requires `kind` to be
unique in `registered_kinds`, which is fine, but the FK then forbids
deleting a `registered_kinds` row while any entity uses it — a startup
ordering nightmare when plugins change between runs. The mechanism is also
heavier than the threat: ADR-022 already enforces manifest-declared kinds
at the `analyze_file` notification boundary (`CLA-INFRA-PLUGIN-UNDECLARED-KIND`
in the writer-actor and in the host's emission-acceptance check). Adding a
data-layer enforcement of a property the protocol layer already enforces
is duplicated work that lands in the wrong place.

**Why rejected**: SQLite CHECK + plugin-extensibility is fundamentally
mismatched; the protocol-layer enforcement is already there.

### Alternative 3: CHECK only where the column is read in a `WHERE` clause

Add CHECK constraints only where a query path discriminates on the
column value (e.g., `WHERE status = 'failed'`); skip CHECKs on columns
whose enum values are only read back, never filtered on.

**Pros**: scopes the defense to where wrong values can produce wrong
results (silent filter mismatches), not where wrong values just look
wrong on inspection.

**Cons**: requires a query-surface audit every time a CHECK decision is
made, which is brittle as the codebase grows. The audit becomes a CHECK
decision at PR-review time, not at schema-design time. The mental cost
of "is this column filtered on?" is bigger than the mental cost of "is
this vocabulary closed?" — and the answer to the second question is
already in an ADR.

**Why rejected**: the policy needs to be cheap to apply, not
optimisation-driven.

### Alternative 4: Defer to v0.2; comment the current schema with TODO links to this audit

Skip the schema edit; document the policy gap; add CHECKs when WP4 lands
findings writes and the value sets are exercised against real data.

**Pros**: no Sprint-2 churn for forward-looking columns nobody is writing
to yet. The risk is theoretical until WP4 writes findings.

**Cons**: ADR-024 explicitly named this issue as deferred to its own ADR;
deferring further moves the decision into the same "implicit-policy"
state ADR-031 is trying to retire. The `runs.status` and `entities.kind`
columns *are* being written today, and the audit's "same class of bug
will appear on every enum-typed TEXT column" framing argues against
waiting for the first WP4 instance.

The other cost of deferral: ADR-024 set the precedent that schema
corrections happen in place before external consumers exist; ADR-031
is well-positioned to use that policy now. Once the in-place trigger
fires, every CHECK addition costs a stacked migration with the
SQLite table-rebuild recipe.

**Why rejected**: the cheap window is now; the deferral cost is
permanent.

## Consequences

### Positive

- The same-shape question that the audit framed as a class of bugs is
  answered uniformly at the schema layer. The next reader of
  `0001_initial_schema.sql` sees the CHECK + comment pattern and does
  not have to reconstruct the policy from precedent.
- Defense-in-depth on `runs.status` catches a real typo class: hand-typed
  string literals in writer-actor code (the `'running'` and `'failed'`
  literals at four sites in `writer.rs`). The `RunStatus` enum prevents
  most of these, but not the in-flight `'running'` insert.
- WP4 (findings emission) lands against a schema whose CHECK clauses
  already encode the ADR-004 and ADR-017 vocabularies. The schema
  becomes the executable specification; WP4's `FindingKind` /
  `FindingSeverity` / `FindingStatus` Rust enums must agree with it by
  CI construction.
- The `entities.kind` / `edges.kind` exceptions are documented at the
  schema-comment level, so a future "should we add CHECKs to these?"
  proposal is answered in place by the comment + this ADR, not by
  re-reading ADR-022 and inferring.
- The two existing CHECK precedents (`edges.confidence`,
  `summary_cache.stale_semantic`) are now backed by a written policy
  they exemplify, not by accident.

### Negative

- Schema-level vocabulary additions become a two-step edit: the ADR
  (or amendment) updates the vocabulary, then the schema migration
  updates the CHECK clause. Pre-trigger this is one in-place edit;
  post-trigger this is a stacked migration with the table-rebuild
  recipe. Migration-author docs need to flag this — the cost is small
  per edit but real, and the table-rebuild recipe is verbose.
- A misspelled value in a writer-actor hand-typed literal becomes a
  `SQLITE_CONSTRAINT_CHECK` failure at runtime rather than a silent
  insert. This is the *intended* behaviour but it shifts the failure
  mode from "data quietly wrong" to "run fails." Either path requires
  test coverage; the new path is louder.
- The schema comments add 8–10 lines to `0001_initial_schema.sql`. The
  file grows slightly; readability improves on net.

### Neutral

- The writer-actor remains the canonical Layer-A validator; the CHECK
  clauses do not change its role. The `enforce_edge_contract` function
  and the `RunStatus` typed enum continue to be the normal-path
  enforcement; the CHECK is the safety net.
- Properties / config / stats / evidence JSON columns are out of scope
  here. Their validation story (if any) belongs to a separate decision
  about JSON-schema enforcement at the writer or pre-flight layer.

## Related Decisions

- [ADR-011](./ADR-011-writer-actor-concurrency.md) — names the writer-actor
  as the canonical mutation path; this ADR generalises its validation
  authority into a documented two-layer model.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — defines plugin authority
  over `entities.kind` and `edges.kind`. This ADR honours that boundary by
  *not* adding CHECK constraints to those columns.
- [ADR-024](./ADR-024-guidance-schema-vocabulary.md) — established the
  in-place edit policy for `0001_initial_schema.sql` and named the
  retirement trigger; this ADR uses the same policy and refers to the
  same trigger. ADR-024 § Consequences/Neutral also explicitly named F-13
  as deferred to a follow-up ADR — this one.
- [ADR-028](./ADR-028-edge-confidence-tiers.md) — defines the
  `edges.confidence` vocabulary that the existing CHECK constraint
  encodes; the in-house precedent ADR-031 generalises.
- [ADR-004](./ADR-004-finding-exchange-format.md) — the source of truth for
  `findings.kind` and `findings.status` values.
- [ADR-017](./ADR-017-severity-and-dedup.md) — the source of truth for
  `findings.severity` values (and the `internal_severity` round-trip slot
  that preserves them across Filigree).

## References

- [Skeleton audit](../../superpowers/handoffs/2026-05-03-skeleton-audit.md)
  — F-13 in the Reviewer additions table; the audit framing that
  motivated this ADR.
- `crates/clarion-storage/migrations/0001_initial_schema.sql:80-81, 134`
  — the two existing CHECK precedents this ADR generalises.
- `crates/clarion-storage/src/commands.rs:26-43` — `RunStatus` Rust enum;
  the typed Layer-A validator for `runs.status` writes.
- `crates/clarion-storage/src/writer.rs:322` — the `'running'` hand-typed
  literal at `BeginRun`; the kind of fat-finger the CHECK catches.
- `crates/clarion-storage/src/writer.rs:411` — `enforce_edge_contract`,
  the Layer-A validator for `edges` writes.
- `crates/clarion-storage/tests/schema_apply.rs:538` —
  `edges_confidence_column_rejects_unknown_tier`, the test pattern this
  ADR's tests follow.
- Filigree issue `clarion-fbe50aa6e1` — the audit issue this ADR closes.
- [SQLite documentation: CHECK constraints](https://www.sqlite.org/lang_createtable.html#ckconst)
  and [Making Other Kinds of Table Schema Changes](https://www.sqlite.org/lang_altertable.html#otheralter)
  — the SQL semantics this ADR relies on, and the post-trigger
  table-rebuild recipe that future CHECK changes will use.
