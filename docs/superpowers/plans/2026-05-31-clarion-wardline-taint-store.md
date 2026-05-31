# Clarion as Wardline taint-fact store (SP9) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Clarion a Wardline-specific, per-entity taint-fact store with an HTTP read+write surface on `clarion serve`, so Wardline's `explain_taint` becomes a cheap query instead of a re-analysis.

**Architecture:** A dedicated `wardline_taint_facts` SQLite table (migration `0003`), written through an *optional* ADR-011 writer-actor that `clarion serve` spawns only when the write API is config-enabled (default off). Resolution of Wardline's **pre-composed** dotted qualname to a Clarion `EntityId` is a direct existence lookup (`python:function:<qualname>`) — Wardline owns the normalization (it pre-composes to byte-match Clarion's canonical form per `fixtures/wardline-qualname-normalization.json`), so Clarion does no normalization at resolution time. New HMAC-gated routes under `/api/wardline/*` and `/api/v1/entities/resolve` expose write, read, batch-get, and resolve. `wardline_json` is stored verbatim and opaque.

**Tech Stack:** Rust (axum, rusqlite, tokio mpsc writer-actor), SQLite, HMAC inbound auth (ADR-034). Docs: ADR + federation contract.

---

## Design decisions locked before coding (read once)

These were settled during brainstorming + plan grounding. They are the load-bearing facts the tasks below assume:

1. **Migration number is `0003`** — `0002_briefing_blocked.sql` already exists. The spec's "migration 0002" is superseded by this plan. `CURRENT_SCHEMA_VERSION` bumps `2 → 3`; a compile-time assert in `schema.rs` enforces the bump.
2. **Resolution is a direct lookup, not normalization.** Wardline sends the **pre-composed** dotted `module.qualified_name` (e.g. `auth.tokens.TokenManager.verify`). Clarion builds the candidate `python:function:<qualname>` and checks existence via `existing_entity_ids`. No `module_dotted_name` port to Rust; no `&file=` disambiguator needed for this scheme. The 5 ADR-018 divergence traps (`<locals>`, nested-class chains, `lib.foo`/`app.service` non-src roots, `a.src.b`) are *Wardline's* conformance burden against the fixture; on Clarion's side they become **verbatim-storage** tests (we must not strip or rewrite the composed string). This is the **exact tier**; Flow B's B.2 (`clarion-ca2d26ffbe`) extends it with the heuristic tier and must consume this resolver, not rebuild it.
3. **Methods are `python:function:`** (not a distinct method kind), and the entity-id `canonical_qualified_name` uses all-dot separators. The `::` form in detailed-design §7 (`python:class:auth.tokens::TokenManager`) is **stale**; the normative fixture uses dots. Taint facts are function/method-scoped (request §3), so function-only resolution is sufficient — recorded as a deliberate scope line.
4. **Taint writes are query-time writes, not analyze-run writes** — they use the `query_time_write` actor path (like `UpsertSummaryCache`), not `BeginRun`/`CommitRun`.
5. **Body limit.** The existing HMAC middleware and `RequestBodyLimitLayer` cap bodies at `HTTP_BODY_LIMIT_BYTES = 16 KiB`. Batched taint writes exceed that, so the `/api/wardline/*` routes get a larger cap (`WARDLINE_BODY_LIMIT_BYTES`) and a per-request fact-count cap (`WARDLINE_TAINT_BATCH_MAX`); Wardline chunks client-side, exactly as Filigree splits against `BATCH_MAX_QUERIES`.
6. **`wardline_json` is opaque.** Clarion stores and returns the blob verbatim; it never parses it. `scan_id` and `content_hash_at_compute` are accepted as *separate top-level fields* on each fact (not parsed out of the blob) so they are queryable columns.

## File structure

| File | Responsibility | Tasks |
|---|---|---|
| `docs/clarion/adr/ADR-036-wardline-taint-fact-store.md` | The federation decision + read→read+write shift + not-a-blob-store guard | T0 |
| `crates/clarion-storage/migrations/0003_wardline_taint_facts.sql` | The table DDL | T1 |
| `crates/clarion-storage/src/schema.rs` | Register migration `0003`, bump `CURRENT_SCHEMA_VERSION` | T1 |
| `crates/clarion-storage/src/wardline_taint.rs` | Records + `upsert_taint_fact` + `get_taint_facts` + `resolve_wardline_qualname` | T2, T4 |
| `crates/clarion-storage/src/lib.rs` | Re-export the new module's public items | T2 |
| `crates/clarion-storage/src/commands.rs` | `WriterCmd::UpsertWardlineTaintFact` variant + `WardlineTaintFactRecord` | T3 |
| `crates/clarion-storage/src/writer.rs` | Actor-loop arm dispatching the new command via `query_time_write` | T3 |
| `crates/clarion-mcp/src/config.rs` | `HttpReadConfig.wardline_taint_write: bool` (default false) | T5 |
| `crates/clarion-cli/src/http_read.rs` | New routes, handlers, AppState fields, larger body limit, optional writer plumbing | T5–T8 |
| `crates/clarion-cli/src/serve.rs` | Pass the write-enable knob into `http_read::spawn` | T5 |
| `docs/federation/contracts.md` | Pin the new routes + freshness contract | T9 |

---

## Task 0 (W.0 — `clarion-e1a5971e42`): ADR-036 — the federation decision

**Doc task, no TDD.** ADR files are immutable once Accepted; write it complete and correct the first time.

**Files:**
- Create: `docs/clarion/adr/ADR-036-wardline-taint-fact-store.md`

- [ ] **Step 1: Write the ADR** following the repo's ADR format (see `ADR-035` header shape: `# ADR-036: …`, then `**Status**`, `**Date**`, `**Deciders**`, then Context / Decision / Consequences). It MUST carry, verbatim in spirit:
  - **Status:** Accepted. **Date:** 2026-05-31.
  - **Context:** Wardline SP9 (`wardline/docs/integration/2026-05-30-wardline-clarion-taint-store-requirements.md`) needs a persistent per-entity taint store keyed by Clarion entity. Clarion's HTTP API is read-only today (ADR-014/ADR-034).
  - **Decision:** Clarion builds a **Wardline-specific** per-entity taint-fact store: a dedicated `wardline_taint_facts` table and `/api/wardline/*` routes. This is the **first read+write** use of Clarion's HTTP API. Writes go through an **optional** ADR-011 writer-actor spawned by `clarion serve` only when config-enabled (default off). Resolution is exact-tier direct lookup of Wardline's pre-composed qualname.
  - **The load-bearing guard (quote it):** *"This is not a precedent for a general-purpose cross-product blob store. The next sibling that wants per-entity persistence gets its own named, justified surface or it does not get one."*
  - **Federation analysis:** passes `loom.md` §3–§5 (enrich-only, both solo-useful, no semantic coupling — `wardline_json` is opaque). Recorded as an ADR, **not** a §5 asterisk, because it *passes* the failure test rather than accepting a violation.
  - **Concurrency posture:** cite ADR-011; a write-enabled `serve` and a concurrent `analyze` are not expected to write the same DB simultaneously; cross-process contention is handled by `PRAGMA busy_timeout=5000` + the `clarion-storage::retry` capped-backoff layer; a write that still cannot land fails retryably and Wardline degrades to SP8.
  - **Consequences:** lists migration `0003`, the new routes, the `serve.http.wardline_taint_write` config knob, and that the heuristic resolution tier + the conformance oracle (`scheme=wardline_qualname` over raw file+qualname) remain deferred (Flow B B.2).
  - Reference the design spec `docs/superpowers/specs/2026-05-30-clarion-wardline-taint-store-design.md`.

- [ ] **Step 2: Register it in the ADR index.** Add the ADR-036 row to `docs/clarion/adr/README.md` matching the existing table/list format.

- [ ] **Step 3: Update CLAUDE.md precedence count.** `CLAUDE.md` says "28 are Accepted at 1.0 (ADR-001…ADR-007, ADR-011, ADR-013…ADR-018, ADR-021…ADR-034)". This is a 1.0-scope statement; do **not** rewrite the 1.0 count. Instead confirm no edit is needed (ADR-036 is a 1.1 addition, outside the quoted 1.0 enumeration). If a maintainer wants a 1.1 ADR list later, that is separate. **No code change in this step — just verify and note in the commit body.**

- [ ] **Step 4: Commit**

```bash
git add docs/clarion/adr/ADR-036-wardline-taint-fact-store.md docs/clarion/adr/README.md
git commit -m "docs(adr): ADR-036 — Clarion as Wardline taint-fact store (read+write HTTP)

W.0 (clarion-e1a5971e42). Records the federation verdict (passes loom.md
§3-§5, ADR not asterisk), the read-only->read+write shift, the optional
writer-actor concurrency posture, and the not-a-general-blob-store guard.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 1 (W.1a — `clarion-6cc50d7a1d`): Migration `0003` + schema-version bump

**Files:**
- Create: `crates/clarion-storage/migrations/0003_wardline_taint_facts.sql`
- Modify: `crates/clarion-storage/src/schema.rs:17-41` (MIGRATIONS array + `CURRENT_SCHEMA_VERSION`)
- Test: existing `schema.rs` migration tests + a new round-trip test in `wardline_taint.rs` (T2)

- [ ] **Step 1: Write the migration SQL**

Create `crates/clarion-storage/migrations/0003_wardline_taint_facts.sql`:

```sql
-- Migration 0003: Wardline taint-fact store (SP9, ADR-036).
-- Dedicated, Wardline-owned per-entity table. NOT the schema-reserved
-- `entities.wardline` column (which `analyze` clobbers with NULL on every
-- re-index). `wardline_json` is opaque to Clarion — stored and returned
-- verbatim. `scan_id` and `content_hash_at_compute` are queryable columns
-- supplied by the caller, not parsed out of the blob.
CREATE TABLE wardline_taint_facts (
    entity_id               TEXT PRIMARY KEY
                                 REFERENCES entities(id) ON DELETE CASCADE,
    wardline_json           TEXT NOT NULL,
    scan_id                 TEXT,
    content_hash_at_compute TEXT,
    updated_at              TEXT NOT NULL
) STRICT;
```

> Note: `0001_initial_schema.sql` uses `STRICT` tables — match that. If grepping `0001` shows it does **not** use `STRICT`, drop the `STRICT` keyword to match the house style. Verify with `grep -c STRICT crates/clarion-storage/migrations/0001_initial_schema.sql` before committing.

- [ ] **Step 2: Register the migration and bump the version**

In `crates/clarion-storage/src/schema.rs`, append to the `MIGRATIONS` array (after the `0002_briefing_blocked` entry):

```rust
    Migration {
        version: 3,
        name: "0003_wardline_taint_facts",
        sql: include_str!("../migrations/0003_wardline_taint_facts.sql"),
    },
```

And change line 33:

```rust
pub const CURRENT_SCHEMA_VERSION: u32 = 3;
```

> The `_CURRENT_SCHEMA_VERSION_MATCHES_LAST_MIGRATION` compile-time assert will fail the build if you add the migration without bumping the constant (or vice-versa) — that is the test for this step.

- [ ] **Step 3: Build to verify the compile-time assert passes**

Run: `cargo build -p clarion-storage`
Expected: compiles clean (the const assert is satisfied: highest migration version `3` == `CURRENT_SCHEMA_VERSION`).

- [ ] **Step 4: Run the existing schema/migration tests**

Run: `cargo nextest run -p clarion-storage schema`
Expected: PASS — existing migration-apply tests now apply `0003` too and write `user_version = 3`.

- [ ] **Step 5: Commit**

```bash
git add crates/clarion-storage/migrations/0003_wardline_taint_facts.sql crates/clarion-storage/src/schema.rs
git commit -m "feat(storage): migration 0003 — wardline_taint_facts table (W.1, ADR-036)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2 (W.1b — `clarion-6cc50d7a1d`): Storage module — records, upsert, fetch, resolve

**Files:**
- Create: `crates/clarion-storage/src/wardline_taint.rs`
- Modify: `crates/clarion-storage/src/lib.rs` (add `mod wardline_taint;` + re-exports)
- Test: inline `#[cfg(test)]` module in `wardline_taint.rs`

The resolver and the fetch are read-pool operations; the upsert helper is invoked by the writer-actor (T3). All three live here so the SQL is in one place.

- [ ] **Step 1: Write the failing test for `resolve_wardline_qualname` using the fixture vectors**

Create `crates/clarion-storage/src/wardline_taint.rs` with the test module first. The test seeds entities matching the fixture's `expected_entity_id`s and asserts resolution. Use the **exact** strings from `docs/federation/fixtures/wardline-qualname-normalization.json` `qualified_name_vectors`:

```rust
//! Wardline taint-fact store (SP9, ADR-036). Dedicated per-entity table;
//! `wardline_json` is opaque (stored/returned verbatim). Resolution is the
//! exact tier: Wardline pre-composes its dotted qualname to byte-match
//! Clarion's canonical_qualified_name, so resolution is a direct existence
//! lookup of `python:function:<qualname>`. Heuristic tier is Flow B B.2.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use crate::query::existing_entity_ids;
use crate::{Result, StorageError};

/// Resolution confidence for a qualname → entity lookup. Exact tier only at
/// 1.1; `Heuristic` is reserved for Flow B B.2 (clarion-ca2d26ffbe) which
/// extends THIS resolver — it must not reimplement resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionConfidence {
    Exact,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub entity_id: Option<String>,
    pub confidence: ResolutionConfidence,
    /// Other entity IDs that matched. Always empty in the exact tier.
    pub alternatives: Vec<String>,
}

/// Build the candidate entity id for a Wardline pre-composed qualname.
/// Taint facts are function/method-scoped (request §3); methods are
/// `python:function:` in Clarion's ontology (ADR-022, fixture-confirmed).
fn function_candidate(qualname: &str) -> String {
    format!("python:function:{qualname}")
}

/// Resolve one pre-composed Wardline qualname to a Clarion entity id (exact
/// tier). Returns `Exact` with the id when the entity exists, else `None`.
pub fn resolve_wardline_qualname(conn: &Connection, qualname: &str) -> Result<Resolution> {
    let resolved = resolve_wardline_qualnames(conn, std::slice::from_ref(&qualname.to_owned()))?;
    Ok(resolved.into_iter().next().map(|(_, r)| r).unwrap_or(Resolution {
        entity_id: None,
        confidence: ResolutionConfidence::None,
        alternatives: Vec::new(),
    }))
}

/// Batch resolve. Returns `(qualname, Resolution)` pairs in input order.
pub fn resolve_wardline_qualnames(
    conn: &Connection,
    qualnames: &[String],
) -> Result<Vec<(String, Resolution)>> {
    let candidates: Vec<String> = qualnames.iter().map(|q| function_candidate(q)).collect();
    let found: HashSet<String> = existing_entity_ids(conn, &candidates)?;
    Ok(qualnames
        .iter()
        .zip(candidates)
        .map(|(qualname, candidate)| {
            let resolution = if found.contains(&candidate) {
                Resolution {
                    entity_id: Some(candidate),
                    confidence: ResolutionConfidence::Exact,
                    alternatives: Vec::new(),
                }
            } else {
                Resolution {
                    entity_id: None,
                    confidence: ResolutionConfidence::None,
                    alternatives: Vec::new(),
                }
            };
            (qualname.clone(), resolution)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal entities table + one function entity row per id. Mirrors the
    /// columns `existing_entity_ids` reads (only `id`).
    fn seed(conn: &Connection, ids: &[&str]) {
        conn.execute_batch(
            "CREATE TABLE entities (id TEXT PRIMARY KEY) STRICT;",
        )
        .unwrap();
        for id in ids {
            conn.execute("INSERT INTO entities (id) VALUES (?1)", params![id])
                .unwrap();
        }
    }

    #[test]
    fn resolves_fixture_vectors_exact() {
        let conn = Connection::open_in_memory().unwrap();
        // expected_entity_id values copied verbatim from
        // fixtures/wardline-qualname-normalization.json qualified_name_vectors.
        seed(
            &conn,
            &[
                "python:function:auth.tokens.TokenManager.verify",
                "python:function:auth.tokens.refresh.<locals>.helper",
                "python:function:pkg.sub.mod.Outer.Inner.method",
                "python:function:lib.foo.Service.handle",
                "python:function:myns.pkg.mod.widget",
            ],
        );
        // The resolver looks up the COMPOSED string verbatim — no stripping of
        // '<locals>', no collapsing of the non-src 'lib.foo' root.
        for qualname in [
            "auth.tokens.TokenManager.verify",
            "auth.tokens.refresh.<locals>.helper",
            "pkg.sub.mod.Outer.Inner.method",
            "lib.foo.Service.handle",
            "myns.pkg.mod.widget",
        ] {
            let r = resolve_wardline_qualname(&conn, qualname).unwrap();
            assert_eq!(r.confidence, ResolutionConfidence::Exact, "{qualname}");
            assert_eq!(
                r.entity_id.as_deref(),
                Some(format!("python:function:{qualname}").as_str()),
                "{qualname}"
            );
            assert!(r.alternatives.is_empty());
        }
    }

    #[test]
    fn unknown_qualname_resolves_none() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn, &["python:function:auth.tokens.TokenManager.verify"]);
        let r = resolve_wardline_qualname(&conn, "auth.tokens.does_not_exist").unwrap();
        assert_eq!(r.confidence, ResolutionConfidence::None);
        assert_eq!(r.entity_id, None);
    }

    #[test]
    fn batch_preserves_input_order_and_mixed_results() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn, &["python:function:a.b.c"]);
        let qs = vec!["a.b.c".to_owned(), "x.y.z".to_owned(), "a.b.c".to_owned()];
        let out = resolve_wardline_qualnames(&conn, &qs).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].1.confidence, ResolutionConfidence::Exact);
        assert_eq!(out[1].1.confidence, ResolutionConfidence::None);
        assert_eq!(out[2].1.confidence, ResolutionConfidence::Exact);
    }
}
```

- [ ] **Step 2: Wire the module into the crate**

In `crates/clarion-storage/src/lib.rs`, add `mod wardline_taint;` alongside the other `mod` declarations, and re-export the public items next to the existing `pub use` lines:

```rust
pub use wardline_taint::{
    Resolution, ResolutionConfidence, TaintFact, TaintFactRow, get_taint_facts,
    resolve_wardline_qualname, resolve_wardline_qualnames, upsert_taint_fact,
};
```

> `TaintFact`, `TaintFactRow`, `get_taint_facts`, `upsert_taint_fact` are added in Steps 4–5; declare the re-export now and the build will fail until they exist (that is the next step's signal).

- [ ] **Step 3: Run the resolver tests (they should fail to compile first, then pass)**

Run: `cargo nextest run -p clarion-storage wardline_taint`
Expected: first FAIL to compile (missing `TaintFact` etc. in the re-export). Temporarily comment the not-yet-defined names out of the `pub use` to run *just* the resolver tests, confirm they PASS, then restore the re-export and proceed to Step 4. (Or write Steps 4–5 first if working top-down; the resolver tests are independent of upsert/fetch.)

- [ ] **Step 4: Add the upsert helper (writer-actor calls this)**

Append to `wardline_taint.rs` (before the test module):

```rust
/// A single taint fact to persist. `wardline_json` is opaque to Clarion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintFact {
    pub entity_id: String,
    pub wardline_json: String,
    pub scan_id: Option<String>,
    pub content_hash_at_compute: Option<String>,
    pub updated_at: String,
}

/// Upsert one taint fact (per-entity replace). Idempotent on `entity_id`.
/// Runs on the writer-actor's connection (T3) outside any run transaction.
pub fn upsert_taint_fact(conn: &Connection, fact: &TaintFact) -> Result<()> {
    conn.execute(
        "INSERT INTO wardline_taint_facts \
            (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(entity_id) DO UPDATE SET \
            wardline_json = excluded.wardline_json, \
            scan_id = excluded.scan_id, \
            content_hash_at_compute = excluded.content_hash_at_compute, \
            updated_at = excluded.updated_at",
        params![
            fact.entity_id,
            fact.wardline_json,
            fact.scan_id,
            fact.content_hash_at_compute,
            fact.updated_at,
        ],
    )?;
    Ok(())
}
```

- [ ] **Step 5: Add the fetch helper**

Append:

```rust
/// A fetched taint fact joined with the entity's CURRENT content hash.
/// `current_content_hash` is the freshness signal Wardline compares against
/// the `content_hash_at_compute` stamped inside `wardline_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintFactRow {
    pub entity_id: String,
    pub wardline_json: String,
    pub current_content_hash: Option<String>,
    pub exists: bool,
}

/// Fetch taint facts for a set of already-resolved entity ids. Returns one
/// row per input id; `exists: false` (and `wardline_json: ""`) when no fact
/// is stored. `current_content_hash` is the entity's containing-file blake3,
/// joined from `entities.content_hash`.
pub fn get_taint_facts(conn: &Connection, entity_ids: &[String]) -> Result<Vec<TaintFactRow>> {
    let mut rows = Vec::with_capacity(entity_ids.len());
    for entity_id in entity_ids {
        let fetched = conn
            .query_row(
                "SELECT f.wardline_json, e.content_hash \
                   FROM wardline_taint_facts f \
                   JOIN entities e ON e.id = f.entity_id \
                  WHERE f.entity_id = ?1",
                params![entity_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()
            .map_err(StorageError::from)?;
        match fetched {
            Some((wardline_json, current_content_hash)) => rows.push(TaintFactRow {
                entity_id: entity_id.clone(),
                wardline_json,
                current_content_hash,
                exists: true,
            }),
            None => rows.push(TaintFactRow {
                entity_id: entity_id.clone(),
                wardline_json: String::new(),
                current_content_hash: None,
                exists: false,
            }),
        }
    }
    Ok(rows)
}
```

> **Freshness caveat to verify:** `entities.content_hash` may be `NULL` because Clarion derives the hash lazily at read time (`query.rs:209-211`, via `file_content_hash`), not at write time. If the join returns `NULL` for `content_hash`, the read handler (T7) must derive it from the entity's source file the same way `resolve_file` does, so Wardline gets a real hash. **Check whether `entities.content_hash` is populated post-analyze** with `grep -n "content_hash" crates/clarion-storage/src/writer.rs`. If it is NOT persisted, T7's handler derives it via the existing `resolve_file`/`file_content_hash` path instead of relying on the join column, and `get_taint_facts` returns the stored fact while the handler supplies the live hash. Resolve this before writing T7's handler — it is the freshness contract's correctness point.

- [ ] **Step 6: Add upsert + fetch tests**

Append to the test module:

```rust
    fn seed_with_hash(conn: &Connection, id: &str, hash: Option<&str>) {
        conn.execute(
            "INSERT INTO entities (id, content_hash) VALUES (?1, ?2)",
            params![id, hash],
        )
        .unwrap();
    }

    #[test]
    fn upsert_then_fetch_roundtrips_verbatim() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entities (id TEXT PRIMARY KEY, content_hash TEXT) STRICT; \
             CREATE TABLE wardline_taint_facts ( \
                entity_id TEXT PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE, \
                wardline_json TEXT NOT NULL, scan_id TEXT, \
                content_hash_at_compute TEXT, updated_at TEXT NOT NULL) STRICT;",
        )
        .unwrap();
        seed_with_hash(&conn, "python:function:a.b.c", Some("deadbeef"));
        let blob = r#"{"schema_version":"wardline-taint-1","taint":{"actual_return":"EXTERNAL_RAW"}}"#;
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: "python:function:a.b.c".to_owned(),
                wardline_json: blob.to_owned(),
                scan_id: Some("scan-1".to_owned()),
                content_hash_at_compute: Some("deadbeef".to_owned()),
                updated_at: "2026-05-31T00:00:00.000Z".to_owned(),
            },
        )
        .unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].exists);
        assert_eq!(rows[0].wardline_json, blob, "blob stored verbatim");
        assert_eq!(rows[0].current_content_hash.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn upsert_replaces_per_entity() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entities (id TEXT PRIMARY KEY, content_hash TEXT) STRICT; \
             CREATE TABLE wardline_taint_facts ( \
                entity_id TEXT PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE, \
                wardline_json TEXT NOT NULL, scan_id TEXT, \
                content_hash_at_compute TEXT, updated_at TEXT NOT NULL) STRICT;",
        )
        .unwrap();
        seed_with_hash(&conn, "python:function:a.b.c", None);
        let mk = |json: &str| TaintFact {
            entity_id: "python:function:a.b.c".to_owned(),
            wardline_json: json.to_owned(),
            scan_id: None,
            content_hash_at_compute: None,
            updated_at: "t".to_owned(),
        };
        upsert_taint_fact(&conn, &mk(r#"{"v":1}"#)).unwrap();
        upsert_taint_fact(&conn, &mk(r#"{"v":2}"#)).unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows[0].wardline_json, r#"{"v":2}"#);
    }

    #[test]
    fn fetch_absent_entity_reports_not_exists() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entities (id TEXT PRIMARY KEY, content_hash TEXT) STRICT; \
             CREATE TABLE wardline_taint_facts ( \
                entity_id TEXT PRIMARY KEY REFERENCES entities(id) ON DELETE CASCADE, \
                wardline_json TEXT NOT NULL, scan_id TEXT, \
                content_hash_at_compute TEXT, updated_at TEXT NOT NULL) STRICT;",
        )
        .unwrap();
        let rows = get_taint_facts(&conn, &["python:function:missing".to_owned()]).unwrap();
        assert!(!rows[0].exists);
        assert_eq!(rows[0].wardline_json, "");
    }
```

- [ ] **Step 7: Run all storage tests**

Run: `cargo nextest run -p clarion-storage wardline_taint`
Expected: PASS (resolve + upsert + fetch).

- [ ] **Step 8: Lint + commit**

```bash
cargo fmt --all -- --check
cargo clippy -p clarion-storage --all-targets --all-features -- -D warnings
git add crates/clarion-storage/src/wardline_taint.rs crates/clarion-storage/src/lib.rs
git commit -m "feat(storage): wardline_taint module — resolve/upsert/fetch (W.1)

Exact-tier qualname resolution via direct python:function:<qualname> lookup.
Tests seeded from fixtures/wardline-qualname-normalization.json. Heuristic
tier deferred to Flow B B.2 (clarion-ca2d26ffbe), which extends this resolver.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3 (W.1c — `clarion-6cc50d7a1d`): Writer-actor command for taint upsert

**Files:**
- Modify: `crates/clarion-storage/src/commands.rs` (new `WriterCmd` variant)
- Modify: `crates/clarion-storage/src/writer.rs` (actor-loop arm)
- Test: a writer integration test (follow the pattern of the existing `UpsertSummaryCache` tests; `grep -n "UpsertSummaryCache" crates/clarion-storage/src/writer.rs` and its test module)

- [ ] **Step 1: Add the command variant**

In `crates/clarion-storage/src/commands.rs`, add to the `use` block:

```rust
use crate::wardline_taint::TaintFact;
```

and append a variant to `enum WriterCmd` (after `TouchSummaryCache`):

```rust
    /// Upsert one Wardline taint fact (per-entity replace). Query-time MCP/HTTP
    /// write; does not require an active analyze run. The fact's `entity_id`
    /// must be pre-resolved by the caller (exact tier) — the writer does not
    /// resolve qualnames.
    UpsertWardlineTaintFact {
        fact: Box<TaintFact>,
        ack: Ack<()>,
    },
```

- [ ] **Step 2: Dispatch it in the actor loop**

In `crates/clarion-storage/src/writer.rs`, add an arm to the `match cmd` in `run_actor` (after the `TouchSummaryCache` arm), using the `query_time_write` path (taint writes are not run-scoped):

```rust
            WriterCmd::UpsertWardlineTaintFact { fact, ack } => {
                let res = query_time_write(conn, &mut state, commits_observed, |conn| {
                    crate::wardline_taint::upsert_taint_fact(conn, &fact)
                });
                reply(ack, res);
            }
```

- [ ] **Step 3: Write the failing writer test**

Add to the `writer.rs` test module (mirror the existing summary-cache writer test's harness — it spawns a `Writer`, applies migrations to a temp DB, sends a command via `send_wait`, then reads back on a fresh connection). Test body:

```rust
    #[tokio::test]
    async fn upsert_wardline_taint_fact_persists() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("clarion.db");
        // Apply migrations + seed one entity so the FK is satisfied.
        {
            let mut conn = rusqlite::Connection::open(&db_path).unwrap();
            crate::schema::apply_migrations(&mut conn).unwrap();
            conn.execute(
                "INSERT INTO entities (id, plugin_id, kind, name, short_name, \
                    properties, created_at, updated_at) \
                 VALUES ('python:function:a.b.c', 'python', 'function', 'a.b.c', \
                    'c', '{}', 't', 't')",
                [],
            )
            .unwrap();
        }
        let (writer, handle) = Writer::spawn(db_path.clone(), 64, 16).unwrap();
        writer
            .send_wait(|ack| WriterCmd::UpsertWardlineTaintFact {
                fact: Box::new(crate::wardline_taint::TaintFact {
                    entity_id: "python:function:a.b.c".to_owned(),
                    wardline_json: r#"{"v":1}"#.to_owned(),
                    scan_id: Some("scan-1".to_owned()),
                    content_hash_at_compute: Some("hash".to_owned()),
                    updated_at: "t".to_owned(),
                }),
                ack,
            })
            .await
            .unwrap();
        drop(writer);
        handle.await.unwrap().unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let json: String = conn
            .query_row(
                "SELECT wardline_json FROM wardline_taint_facts WHERE entity_id = ?1",
                ["python:function:a.b.c"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(json, r#"{"v":1}"#);
    }
```

> Verify the exact `entities` INSERT column list against `0001_initial_schema.sql` (`grep -n "CREATE TABLE entities" -A40 crates/clarion-storage/migrations/0001_initial_schema.sql`) and adjust the seed INSERT to satisfy NOT NULL columns. Reuse the existing writer test's entity-seed helper if one exists.

- [ ] **Step 4: Run it**

Run: `cargo nextest run -p clarion-storage upsert_wardline_taint_fact_persists`
Expected: PASS.

- [ ] **Step 5: Full storage gate + commit**

```bash
cargo clippy -p clarion-storage --all-targets --all-features -- -D warnings
cargo nextest run -p clarion-storage
git add crates/clarion-storage/src/commands.rs crates/clarion-storage/src/writer.rs
git commit -m "feat(storage): WriterCmd::UpsertWardlineTaintFact (W.1)

Query-time write path (query_time_write), entity_id pre-resolved by caller.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4 (W.4 — `clarion-2f9ad948e5`): Resolve endpoint `POST /api/wardline/resolve`

> **Filigree dependency fix:** W.4 currently depends only on W.0. It also depends on **W.1** (it calls `resolve_wardline_qualnames` from T2). Add the edge `W.4 → W.1` (`clarion-2f9ad948e5` depends on `clarion-6cc50d7a1d`) before/at execution. Also drop a one-line comment on B.2 (`clarion-ca2d26ffbe`): "Exact-tier qualname resolver shipped in W.1 (`clarion-storage::wardline_taint::resolve_wardline_qualnames`); B.2 extends it with the heuristic tier — do not reimplement."

This task is sequenced **before** the write/read endpoints because they share the route-registration + larger-body-limit scaffolding it introduces, and W.2 reuses the resolver. (W.4's filigree issue can still close independently.)

**Files:**
- Modify: `crates/clarion-cli/src/http_read.rs` (route, handler, request/response types, ErrorCode additions, body-limit constant, wardline sub-router)

- [ ] **Step 1: Add the larger body limit + batch cap constants**

Near the existing `const BATCH_MAX_QUERIES`/`HTTP_BODY_LIMIT_BYTES` block in `http_read.rs`:

```rust
/// Body limit for the Wardline taint-store routes. Batched writes/resolves
/// carry thousands of qualnames; the 16 KiB read-API limit is far too small.
/// Wardline chunks client-side against WARDLINE_TAINT_BATCH_MAX (mirrors how
/// Filigree splits against BATCH_MAX_QUERIES). Pinned in contracts.md (W.5).
const WARDLINE_BODY_LIMIT_BYTES: usize = 4 * 1024 * 1024;
/// Max qualnames/facts in one Wardline request.
const WARDLINE_TAINT_BATCH_MAX: usize = 2000;
```

- [ ] **Step 2: Add request/response types + new ErrorCode variants**

Add ErrorCode variants to the existing enum: `WriteDisabled`, `ProjectMismatch`. Add types:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    #[serde(default)]
    project: String,
    qualnames: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ResolveResponse {
    /// qualname -> entity_id, only for exact matches.
    resolved: std::collections::BTreeMap<String, String>,
    unresolved: Vec<String>,
}
```

- [ ] **Step 3: Write the handler**

```rust
async fn post_wardline_resolve(
    State(state): State<AppState>,
    body: Result<Json<ResolveRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(StatusCode::BAD_REQUEST, ErrorCode::InvalidPath, &rej.body_text());
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.qualnames.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many qualnames in one request",
        );
    }
    let readers = state.readers.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = readers.get()?;
        clarion_storage::resolve_wardline_qualnames(&conn, &req.qualnames)
    })
    .await;
    match result {
        Ok(Ok(pairs)) => {
            let mut resolved = std::collections::BTreeMap::new();
            let mut unresolved = Vec::new();
            for (qualname, resolution) in pairs {
                match resolution.entity_id {
                    Some(id) => {
                        resolved.insert(qualname, id);
                    }
                    None => unresolved.push(qualname),
                }
            }
            (StatusCode::OK, Json(ResolveResponse { resolved, unresolved })).into_response()
        }
        Ok(Err(err)) => storage_error_response(&err),
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "resolve task panicked",
        ),
    }
}
```

> Verify the exact `ReaderPool` accessor (`readers.get()` vs another name) and the existing storage→Response mapping helper by reading how `get_file`/`post_files_resolve` obtain a connection and map `StorageError`. Reuse that helper (named here `storage_error_response`) rather than inventing one; if no such helper exists, mirror the inline mapping `get_file` uses. Also confirm the `Json` extractor + `JsonRejection` import path matches the crate's axum version.

- [ ] **Step 4: Add `reject_project_mismatch` to AppState**

The `project` field is a **guard** (must match the served project), per Decision 6. Add to `impl AppState` (project name = the project root's file name, or an explicit configured project id — verify which Clarion uses; the served project is one DB under one root):

```rust
impl AppState {
    /// The `project` request field is a guard, not a selector: one `serve`
    /// serves exactly one project. An empty field is permitted (Wardline may
    /// omit it); a non-empty mismatch is rejected.
    fn reject_project_mismatch(&self, requested: &str) -> Option<Response> {
        if requested.is_empty() {
            return None;
        }
        let served = self
            .project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if requested == served {
            None
        } else {
            Some(json_error(
                StatusCode::FORBIDDEN,
                ErrorCode::ProjectMismatch,
                "project guard mismatch: this server serves a different project",
            ))
        }
    }
}
```

> **Decision to confirm:** what string is the canonical project handle Wardline will send? Options: the project-root dir name, the `instance_id`, or a configured `project_id`. Wardline's `project` field is opaque to it; the contract (W.5) must pin one. Default to the project-root dir name for v1 (cheapest, no new config) and pin it in contracts.md; if `instance_id` is preferred for stability, compare against `state.instance_id` instead. Pick one, encode it here, document it in W.5.

- [ ] **Step 5: Register the route on a wardline sub-router with the larger body limit**

Refactor `fn router(state: AppState)` to add a second protected sub-router for the wardline routes carrying `WARDLINE_BODY_LIMIT_BYTES`. The wardline routes use HMAC the same way; the difference is the body limit. Add to `router()`:

```rust
    let wardline = Router::new()
        .route("/api/wardline/resolve", post(post_wardline_resolve))
        // taint-facts write/read routes added in T5/T6/T7
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_http_identity_wardline,
        ))
        .layer(RequestBodyLimitLayer::new(WARDLINE_BODY_LIMIT_BYTES));
    protected.merge(unprotected).merge(wardline).with_state(state).layer(/* unchanged outer stack */)
```

> **HMAC body-limit coupling:** `require_hmac_identity` reads the body with `to_bytes(body, HTTP_BODY_LIMIT_BYTES)` (hardcoded 16 KiB) to verify the signature, then reconstructs the request. For wardline routes that limit must be `WARDLINE_BODY_LIMIT_BYTES` or large bodies fail HMAC before reaching the handler. **Make the limit a parameter:** change `require_hmac_identity(secret, request, next)` to take a `body_limit: usize`, add a thin `require_http_identity_wardline` wrapper that calls it with `WARDLINE_BODY_LIMIT_BYTES`, and keep the existing `require_http_identity` passing `HTTP_BODY_LIMIT_BYTES`. Verify the exact `to_bytes` call site (`http_read.rs:~441`) and thread the parameter through.

- [ ] **Step 6: Write an HTTP-level test**

Add an integration test (mirror the existing http_read tests — `grep -n "#\[tokio::test\]\|fn router\|spawn_with_env\|reqwest\|TestServer\|oneshot" crates/clarion-cli/src/http_read.rs` to find the in-process test harness). The test builds a `router(state)` over a temp DB seeded with `python:function:a.b.c`, POSTs `{"qualnames":["a.b.c","x.y.z"]}` with a valid HMAC header, and asserts `resolved == {"a.b.c": "python:function:a.b.c"}` and `unresolved == ["x.y.z"]`. If the file has no in-process HTTP harness, add the test under `crates/clarion-cli/tests/` as an integration test that boots `spawn_with_env` on `127.0.0.1:0` and uses a stdlib/`reqwest` client (check `Cargo.toml` dev-deps for an HTTP client; `wp2_e2e` tests show the established pattern).

- [ ] **Step 7: Run + gate + commit**

```bash
cargo nextest run -p clarion-cli wardline_resolve
cargo clippy -p clarion-cli --all-targets --all-features -- -D warnings
git add crates/clarion-cli/src/http_read.rs
git commit -m "feat(serve): POST /api/wardline/resolve — exact-tier qualname resolve (W.4)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5 (W.2a — `clarion-96e80a907c`): Config knob + optional writer-actor on `serve`

**Files:**
- Modify: `crates/clarion-mcp/src/config.rs` (`HttpReadConfig.wardline_taint_write`)
- Modify: `crates/clarion-cli/src/serve.rs` (pass the knob into spawn)
- Modify: `crates/clarion-cli/src/http_read.rs` (spawn an optional `Writer` inside the HTTP runtime; store its sender in `AppState`)

- [ ] **Step 1: Add the config field (failing config test)**

In `crates/clarion-mcp/src/config.rs`, add to `HttpReadConfig`:

```rust
    /// Enable the Wardline taint-store WRITE API (POST /api/wardline/taint-facts).
    /// Default false — `serve` is read-only unless explicitly opted in (ADR-036).
    /// When true, `serve` spawns an optional ADR-011 writer-actor.
    #[serde(default)]
    pub wardline_taint_write: bool,
```

and `wardline_taint_write: false` in the `Default` impl. Add/extend a config-parse test asserting the default is `false` and that `wardline_taint_write: true` in YAML parses.

Run: `cargo nextest run -p clarion-mcp config`
Expected: PASS.

- [ ] **Step 2: Thread the knob + an optional writer sender into `AppState`**

Add to `struct AppState`:

```rust
    /// Present only when serve.http.wardline_taint_write is true (ADR-036).
    /// `None` ⇒ the write API is disabled and returns 403 WRITE_DISABLED.
    taint_writer: Option<tokio::sync::mpsc::Sender<clarion_storage::WriterCmd>>,
```

- [ ] **Step 3: Spawn the optional writer inside the HTTP runtime**

In `run_http_read_server`, after building the runtime and before constructing `AppState`, spawn a `Writer` when enabled. The writer must live on the HTTP runtime (its actor uses `spawn_blocking`); keep its `JoinHandle` and await it on graceful shutdown. Add a `wardline_taint_write: bool` parameter threaded from `spawn`/`spawn_with_env` (from `config.serve.http.wardline_taint_write`) down to `run_http_read_server`.

```rust
        let (taint_writer, taint_writer_join) = if wardline_taint_write {
            let (writer, join) = clarion_storage::Writer::spawn(
                db_path.clone(),
                clarion_storage::DEFAULT_BATCH_SIZE,
                clarion_storage::DEFAULT_CHANNEL_CAPACITY,
            )
            .map_err(|err| anyhow!("spawn taint writer-actor: {err}"))?;
            (Some(writer.sender()), Some(join))
        } else {
            (None, None)
        };
```

> `run_http_read_server` does not currently receive `db_path` — it gets `readers`. Thread `db_path: PathBuf` through `spawn`/`spawn_with_env`/`run_http_read_server` (it is available in `serve.rs` as `db_path`). On graceful shutdown (after `serve_future` completes), `drop` the writer sender and `await` `taint_writer_join` so the actor flushes. Add that to the shutdown path; the `Writer::spawn` JoinHandle returns `Result<()>` — surface its error.

- [ ] **Step 4: Pass the knob from `serve.rs`**

In `crates/clarion-cli/src/serve.rs`, the `http_read::spawn(...)` call gains the project's `db_path` (already computed at the top of `run`) — pass it, and the spawn reads `wardline_taint_write` from `&config.serve.http`. Update the `spawn` signature/call accordingly. Confirm the `Arc::ptr_eq` reader-identity assert still holds (the writer uses a *separate* connection, unrelated to the reader pool identity check).

- [ ] **Step 5: Build the workspace**

Run: `cargo build --workspace --bins`
Expected: compiles. (No behavior test yet — exercised by T6.)

- [ ] **Step 6: Commit**

```bash
git add crates/clarion-mcp/src/config.rs crates/clarion-cli/src/serve.rs crates/clarion-cli/src/http_read.rs
git commit -m "feat(serve): optional writer-actor + wardline_taint_write config (W.2)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6 (W.2b — `clarion-96e80a907c`): Write endpoint `POST /api/wardline/taint-facts`

**Files:**
- Modify: `crates/clarion-cli/src/http_read.rs`

- [ ] **Step 1: Request/response types**

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaintFactInput {
    qualname: String,
    /// Opaque blob, stored verbatim. Accept any JSON value and re-serialize.
    wardline_json: serde_json::Value,
    #[serde(default)]
    scan_id: Option<String>,
    #[serde(default)]
    content_hash_at_compute: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteTaintFactsRequest {
    #[serde(default)]
    project: String,
    #[serde(default)]
    scan_id: Option<String>,
    facts: Vec<TaintFactInput>,
}

#[derive(Debug, Serialize)]
struct WriteTaintFactsResponse {
    written: usize,
    unresolved_qualnames: Vec<String>,
}
```

> `content_hash_at_compute` and `scan_id` may also live inside the blob (per the Wardline §5 shape). Accept the **top-level** fields as the queryable columns; if absent, fall back to `req.scan_id`. Do NOT parse them out of `wardline_json` (opaque contract). Document in W.5 that Wardline should send them top-level for the columns.

- [ ] **Step 2: Handler — resolve exact-only, then upsert each resolved fact**

```rust
async fn post_wardline_taint_facts(
    State(state): State<AppState>,
    body: Result<Json<WriteTaintFactsRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some(writer) = state.taint_writer.clone() else {
        return json_error(
            StatusCode::FORBIDDEN,
            ErrorCode::WriteDisabled,
            "taint-fact write API is disabled (set serve.http.wardline_taint_write: true)",
        );
    };
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => return json_error(StatusCode::BAD_REQUEST, ErrorCode::InvalidPath, &rej.body_text()),
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.facts.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, ErrorCode::BatchTooLarge, "too many facts in one request");
    }

    // Resolve all qualnames (exact tier) on the read pool first.
    let qualnames: Vec<String> = req.facts.iter().map(|f| f.qualname.clone()).collect();
    let readers = state.readers.clone();
    let resolved = match tokio::task::spawn_blocking(move || {
        let conn = readers.get()?;
        clarion_storage::resolve_wardline_qualnames(&conn, &qualnames)
    })
    .await
    {
        Ok(Ok(pairs)) => pairs,
        Ok(Err(err)) => return storage_error_response(&err),
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, "resolve task panicked"),
    };

    let now = clarion_storage::now_iso8601(); // verify the project's ISO-8601 helper name/path
    let mut written = 0usize;
    let mut unresolved = Vec::new();
    for (fact_input, (_, resolution)) in req.facts.iter().zip(resolved) {
        let Some(entity_id) = resolution.entity_id else {
            unresolved.push(fact_input.qualname.clone());
            continue;
        };
        let fact = clarion_storage::TaintFact {
            entity_id,
            wardline_json: fact_input.wardline_json.to_string(),
            scan_id: fact_input.scan_id.clone().or_else(|| req.scan_id.clone()),
            content_hash_at_compute: fact_input.content_hash_at_compute.clone(),
            updated_at: now.clone(),
        };
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        if writer
            .send(clarion_storage::WriterCmd::UpsertWardlineTaintFact { fact: Box::new(fact), ack: ack_tx })
            .await
            .is_err()
        {
            return json_error(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::StorageError, "writer unavailable");
        }
        match ack_rx.await {
            Ok(Ok(())) => written += 1,
            Ok(Err(err)) => return storage_error_response(&err),
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, "writer dropped ack"),
        }
    }
    (StatusCode::OK, Json(WriteTaintFactsResponse { written, unresolved_qualnames: unresolved })).into_response()
}
```

> Verify: (a) the ISO-8601 timestamp helper — search `grep -rn "strftime\|iso8601\|Utc::now\|fn now" crates/clarion-storage/src crates/clarion-cli/src`; the writer-actor elsewhere uses `strftime('%Y-%m-%dT%H:%M:%fZ','now')` in SQL or a caller-supplied timestamp. Supply the timestamp caller-side (handlers run on the HTTP runtime where `chrono`/`time` may already be a dep — check `Cargo.toml`). If no helper exists, format with the same pattern the analyze path uses. (b) `WriterCmd`/`TaintFact` are re-exported from `clarion_storage` (added in T2/T3 — confirm the `pub use` includes `WriterCmd`; it is already public via `commands`, but check the crate root re-export and add it if missing).

- [ ] **Step 3: Register the route** on the `wardline` sub-router (T4 Step 5):

```rust
        .route("/api/wardline/taint-facts", post(post_wardline_taint_facts))
```

- [ ] **Step 4: Tests** (in-process router or integration harness from T4 Step 6):
  - write disabled (`taint_writer: None`) → 403 `WRITE_DISABLED`.
  - write enabled, batch with one resolvable + one unresolvable qualname → `written: 1`, `unresolved_qualnames: ["..."]`, and the stored blob round-trips verbatim via a follow-up DB read.
  - per-entity replace: write the same qualname twice → second overwrites.
  - `project` guard mismatch → 403 `PROJECT_MISMATCH`.
  - oversize batch (`facts.len() > WARDLINE_TAINT_BATCH_MAX`) → 413.

- [ ] **Step 5: Run + gate + commit**

```bash
cargo nextest run -p clarion-cli wardline_taint
cargo clippy -p clarion-cli --all-targets --all-features -- -D warnings
git add crates/clarion-cli/src/http_read.rs
git commit -m "feat(serve): POST /api/wardline/taint-facts — exact-only batch write (W.2)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7 (W.3 — `clarion-1c1a17f0f0`): Read endpoints — single + batch-get

**Files:**
- Modify: `crates/clarion-cli/src/http_read.rs`

- [ ] **Step 1: Resolve the freshness/`current_content_hash` source** (the open item flagged in T2 Step 5). If `entities.content_hash` is populated post-analyze, `get_taint_facts`' join is sufficient. If it is `NULL` (lazy derivation), the handler must derive the live hash per entity via the existing `resolve_file`/`file_content_hash` path before responding. Confirm now; the handler below assumes `get_taint_facts` returns the hash and adds a derivation fallback when it is `None`.

- [ ] **Step 2: Types + single-fetch handler**

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaintFactQuery {
    #[serde(default)]
    project: String,
    qualname: String,
}

#[derive(Debug, Serialize)]
struct TaintFactView {
    qualname: String,
    wardline_json: Option<serde_json::Value>,
    current_content_hash: Option<String>,
    exists: bool,
}

async fn get_wardline_taint_fact(
    State(state): State<AppState>,
    query: Result<Query<TaintFactQuery>, QueryRejection>,
) -> Response {
    let Query(q) = match query {
        Ok(query) => query,
        Err(rej) => return json_error(StatusCode::BAD_REQUEST, ErrorCode::InvalidPath, &rej.body_text()),
    };
    if let Some(resp) = state.reject_project_mismatch(&q.project) {
        return resp;
    }
    respond_taint_facts(&state, vec![q.qualname]).await.map_or_else(
        |resp| resp,
        |mut views| (StatusCode::OK, Json(views.remove(0))).into_response(),
    )
}
```

- [ ] **Step 3: Batch-get handler + shared `respond_taint_facts`**

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchGetRequest {
    #[serde(default)]
    project: String,
    qualnames: Vec<String>,
}

async fn post_wardline_taint_facts_batch_get(
    State(state): State<AppState>,
    body: Result<Json<BatchGetRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => return json_error(StatusCode::BAD_REQUEST, ErrorCode::InvalidPath, &rej.body_text()),
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.qualnames.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, ErrorCode::BatchTooLarge, "too many qualnames");
    }
    respond_taint_facts(&state, req.qualnames).await.map_or_else(
        |resp| resp,
        |views| (StatusCode::OK, Json(views)).into_response(),
    )
}

/// Resolve qualnames → entity ids, fetch facts, build views. Returns the
/// views on success or a ready Response on error. A qualname that does not
/// resolve yields `exists: false` (treated identically to "no fact stored").
async fn respond_taint_facts(
    state: &AppState,
    qualnames: Vec<String>,
) -> std::result::Result<Vec<TaintFactView>, Response> {
    let readers = state.readers.clone();
    let qn = qualnames.clone();
    let fetched = tokio::task::spawn_blocking(move || -> clarion_storage::Result<Vec<(String, clarion_storage::Resolution, Option<clarion_storage::TaintFactRow>)>> {
        let conn = readers.get()?;
        let resolved = clarion_storage::resolve_wardline_qualnames(&conn, &qn)?;
        let resolved_ids: Vec<String> = resolved.iter().filter_map(|(_, r)| r.entity_id.clone()).collect();
        let rows = clarion_storage::get_taint_facts(&conn, &resolved_ids)?;
        // zip rows back to qualnames by entity id
        let by_id: std::collections::HashMap<String, clarion_storage::TaintFactRow> =
            rows.into_iter().map(|r| (r.entity_id.clone(), r)).collect();
        Ok(resolved
            .into_iter()
            .map(|(qualname, resolution)| {
                let row = resolution.entity_id.as_ref().and_then(|id| by_id.get(id).cloned());
                (qualname, resolution, row)
            })
            .collect())
    })
    .await;

    let triples = match fetched {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => return Err(storage_error_response(&err)),
        Err(_) => return Err(json_error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, "fetch task panicked")),
    };
    let views = triples
        .into_iter()
        .map(|(qualname, _resolution, row)| match row {
            Some(r) if r.exists => TaintFactView {
                qualname,
                wardline_json: serde_json::from_str(&r.wardline_json).ok(),
                current_content_hash: r.current_content_hash,
                exists: true,
            },
            _ => TaintFactView { qualname, wardline_json: None, current_content_hash: None, exists: false },
        })
        .collect();
    Ok(views)
}
```

> If T7 Step 1 finds `content_hash` is lazily derived (NULL in the table), augment the `spawn_blocking` closure to derive each entity's current hash via the existing `resolve_file`-style path and populate `current_content_hash` there, so a stored-but-NULL-column fact still returns a live hash.

- [ ] **Step 4: Register both routes** on the `wardline` sub-router:

```rust
        .route("/api/wardline/taint-facts", get(get_wardline_taint_fact))
        .route("/api/wardline/taint-facts:batch-get", post(post_wardline_taint_facts_batch_get))
```

> Note `/api/wardline/taint-facts` now has both `get` (this task) and `post` (T6). Register them on the same path with `get(...).post(...)` or `.route("/api/wardline/taint-facts", get(get_wardline_taint_fact).post(post_wardline_taint_facts))`. Adjust T6 Step 3 accordingly when merging.

- [ ] **Step 5: Tests:**
  - fetch a stored fact → `exists: true`, `wardline_json` round-trips, `current_content_hash` present.
  - fetch an absent/unresolvable qualname → `exists: false`, no fabricated blob.
  - batch-get mixed present/absent in one call → one round-trip, correct per-entity flags.
  - reads work with the write API **disabled** (`taint_writer: None`) — reads use the pool, not the writer.

- [ ] **Step 6: Run + gate + commit**

```bash
cargo nextest run -p clarion-cli wardline_taint
cargo clippy -p clarion-cli --all-targets --all-features -- -D warnings
git add crates/clarion-cli/src/http_read.rs
git commit -m "feat(serve): GET + :batch-get /api/wardline/taint-facts (W.3)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Full-workspace gate (all WP code landed)

- [ ] **Step 1: Run the complete ADR-023 gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

Expected: all green. Fix anything red before proceeding.

- [ ] **Step 2: Capabilities advertise the write surface (optional but recommended).** If `GET /api/v1/_capabilities` (`CapabilitiesResponse`) is how siblings probe, add a `wardline_taint_store: bool` field reflecting `state.taint_writer.is_some()` so Wardline can detect the write API pre-auth/pre-write. Add a test. Commit separately.

---

## Task 9 (W.5 — `clarion-3ccf996c12`): Pin the contract in `contracts.md`

**Doc task, no TDD.** Depends on W.2 + W.3 (+ W.4) shipped so the documented shapes match reality.

**Files:**
- Modify: `docs/federation/contracts.md`
- Optionally create: `docs/federation/fixtures/post-api-wardline-taint-facts.json` etc. (mirror the existing fixture style)

- [ ] **Step 1: Add a "Wardline taint-fact store (SP9)" section** to `contracts.md` pinning, verbatim against the implemented handlers:
  - **Routes:** `POST /api/wardline/resolve`, `POST /api/wardline/taint-facts` (write), `GET /api/wardline/taint-facts?project=&qualname=`, `POST /api/wardline/taint-facts:batch-get`. All HMAC-gated (`X-Loom-Component: clarion:<hmac>`, ADR-034).
  - **Qualname keying:** pre-composed dotted `module.qualified_name` (byte-faithful to `module_dotted_name()` + `__qualname__`); conformance is the `wardline-qualname-normalization.json` fixture. Resolution is exact-tier; unresolved qualnames are returned, never guessed. Writes require `exact`.
  - **Body/batch limits:** `WARDLINE_BODY_LIMIT_BYTES` (4 MiB) and `WARDLINE_TAINT_BATCH_MAX` (2000 facts/qualnames per request); Wardline chunks client-side. State the exact numbers.
  - **Freshness contract:** fetch returns `current_content_hash` (blake3 of the containing file's raw bytes, hex, whole-file — **not** sha256/LF-normalized); Wardline compares it to the `content_hash_at_compute` it stamped inside `wardline_json`; match → fresh, mismatch/`exists:false` → recompute. Clarion never asserts freshness.
  - **`project` guard:** the handle is the project-root dir name (or whatever T4 Step 4 settled); a non-empty mismatch → 403 `PROJECT_MISMATCH`; empty is accepted.
  - **Opacity:** `wardline_json` stored/returned verbatim; `scan_id`/`content_hash_at_compute` sent top-level become queryable columns; Clarion never parses the blob.
  - **Write-disabled posture:** writes → 403 `WRITE_DISABLED` unless `serve.http.wardline_taint_write: true`; reads always available.
  - **Error codes:** enumerate the new `ErrorCode`s (`WRITE_DISABLED`, `PROJECT_MISMATCH`) alongside the existing list so federation clients route on `code`.

- [ ] **Step 2: Cross-link** the design spec and ADR-036 from the new section, and note the heuristic tier + conformance oracle remain deferred (Flow B B.2).

- [ ] **Step 3: Commit**

```bash
git add docs/federation/contracts.md docs/federation/fixtures/
git commit -m "docs(federation): pin Wardline taint-store routes + freshness contract (W.5)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Close the loop

- [ ] **Step 1: Close the filigree issues** as each lands (W.0→W.1→W.2/W.3/W.4→W.5), citing ADR-036. Walk each issue's workflow to its terminal state.
- [ ] **Step 2: Update the umbrella** `clarion-0d7a342c1b` with a closing comment summarizing what shipped and what is deferred (heuristic tier, conformance oracle, prune-by-scan).
- [ ] **Step 3: Comment on B.2** (`clarion-ca2d26ffbe`) and Flow B umbrella that the exact-tier resolver now exists and must be extended, not rebuilt.

---

## Self-review

**Spec coverage** (design spec §3 decisions + §4 architecture + §8 issues):
- Decision 1 (qualname-keyed, exact-only write) → T2 resolver + T6 write handler. ✓
- Decision 2 (`scan_id` column) → T1 schema + T2 `TaintFact` + T6 top-level field. ✓
- Decision 3 (blake3 content hash, per-entity on fetch) → T2 `get_taint_facts` join + T7 freshness derivation + W.5 doc. ✓
- Decision 4 (no cascade/prune; freshness gate) → FK `ON DELETE CASCADE` as defense-in-depth (T1) + `exists:false` path (T7); no prune call built. ✓
- Decision 5 (HTTP+JSON, HMAC, read+write) → T4–T7 routes on the HMAC-gated wardline sub-router. ✓
- Decision 6 (project guard) → T4 `reject_project_mismatch`. ✓
- §4.1 optional writer-actor on serve → T5. ✓
- §4.2 dedicated table (not the `wardline` column) → T1. ✓
- §4.4 endpoints incl. resolve oracle → T4 (resolve), T6 (write), T7 (read/batch-get). ✓
- §8 W.0–W.5 → T0, T1–T3 (W.1), T5–T6 (W.2), T7 (W.3), T4 (W.4), T9 (W.5). ✓
- Non-goals (§9): no blob parsing, no SP8 replacement, no general blob store — preserved; heuristic tier + conformance oracle explicitly deferred. ✓

**Open items deliberately deferred to execution-time verification (each flagged inline, none block the next task's start):**
1. `STRICT` table keyword — match `0001` (T1 Step 1).
2. `entities.content_hash` populated vs lazily derived — drives T7's freshness source (T2 Step 5 / T7 Step 1).
3. ReaderPool connection accessor name + `StorageError`→Response helper name — read `get_file` (T4 Step 3).
4. ISO-8601 timestamp helper (T6 Step 2).
5. The canonical `project` handle string (T4 Step 4) — pinned in W.5.
6. In-process HTTP test harness vs integration test (T4 Step 6).

**Type consistency:** `TaintFact` (storage write record, T2), `TaintFactRow` (storage fetch row, T2), `TaintFactInput`/`TaintFactView` (HTTP wire types, T6/T7), `Resolution`/`ResolutionConfidence` (T2), `WriterCmd::UpsertWardlineTaintFact` (T3). Re-exports added in T2 Step 2 cover `resolve_wardline_qualname(s)`, `get_taint_facts`, `upsert_taint_fact`, `TaintFact`, `TaintFactRow`, `Resolution`, `ResolutionConfidence`; `WriterCmd` re-export verified in T6 Step 2. Endpoint paths consistent across T4/T6/T7 and W.5.

**Filigree dependency correction:** add edge **W.4 → W.1** (resolve oracle consumes the W.1 resolver); leave W.2→W.1, W.3→W.1, W.5→{W.2,W.3} as-is. Do NOT make W.2 depend on W.4 (the oracle is HTTP-over-the-resolver, not its owner).
