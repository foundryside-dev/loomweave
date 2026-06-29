//! GV-LW-2 conformance + honest-degrade for the Warpline churn consumer.
//!
//! The dead `entity_high_churn_list` / `entity_recent_change_list` surfaces are
//! lit up by consuming Warpline's FROZEN `warpline_entity_churn_count_get`
//! (`warpline.entity_churn_count.v1`) at read time (2026-06-13 interface lock
//! §1A / GV-LW-2). These tests inject a FAKE `WarplineLookup` that parses the
//! frozen *envelope* fixture through the real parse path — they NEVER make a
//! live Warpline MCP call (a hub-rooted session would misroute it; the contract
//! shape is what conformance pins).
//!
//! GV-LW-2 (lock §1D): input 3 SEIs, one never-observed → `data.items` len 3,
//! the observed two carry `churn_count >= 1`, the unobserved carries
//! `churn_count: 0` (not omitted, not error).

use std::sync::{Arc, Mutex};

use loomweave_mcp::ServerState;
use loomweave_mcp::warpline::{
    ChurnCountResponse, WarplineClientError, WarplineEntityRef, WarplineLookup,
    parse_churn_count_response,
};
use loomweave_storage::{ReaderPool, pragma, schema};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

// The three SEIs of the GV-LW-2 vector. alpha + beta are observed by warpline;
// gamma is never-observed.
const SEI_ALPHA: &str = "loomweave:eid:0000000000000000000000000000000a";
const SEI_BETA: &str = "loomweave:eid:0000000000000000000000000000000b";
const SEI_GAMMA: &str = "loomweave:eid:0000000000000000000000000000000c";

const LOC_ALPHA: &str = "python:function:src/pkg/mod.py::alpha";
const LOC_BETA: &str = "python:function:src/pkg/mod.py::beta";
const LOC_GAMMA: &str = "python:function:src/pkg/mod.py::gamma";

/// The recorded FROZEN `warpline.entity_churn_count.v1` envelope — the GV-LW-2
/// producer fixture (full envelope, not a convenient subset). alpha=7, beta=2,
/// gamma=0 (present, not omitted).
const GV_LW_2_ENVELOPE: &str = r#"{
  "schema": "warpline.entity_churn_count.v1",
  "ok": true,
  "query": {"repo": "/abs/path", "tool": "warpline_entity_churn_count_get",
            "arguments": {}, "filters": {}, "sort": {"by": "churn_count", "order": "desc"},
            "page": {"limit": 100, "cursor": null}},
  "data": {
    "items": [
      {"entity": {"sei": "loomweave:eid:0000000000000000000000000000000a",
                  "locator": "python:function:src/pkg/mod.py::alpha"},
       "churn_count": 7, "first_changed_at": "2026-05-01T00:00:00Z",
       "last_changed_at": "2026-06-13T00:00:00Z", "last_actor": "agent:codex"},
      {"entity": {"sei": "loomweave:eid:0000000000000000000000000000000b",
                  "locator": "python:function:src/pkg/mod.py::beta"},
       "churn_count": 2, "first_changed_at": "2026-05-10T00:00:00Z",
       "last_changed_at": "2026-06-01T00:00:00Z", "last_actor": "agent:fable"},
      {"entity": {"sei": "loomweave:eid:0000000000000000000000000000000c",
                  "locator": "python:function:src/pkg/mod.py::gamma"},
       "churn_count": 0, "first_changed_at": null, "last_changed_at": null, "last_actor": null}
    ],
    "window": {"since": null, "until": null, "rev_range": null},
    "page": {"limit": 100, "next_cursor": null, "has_more": false}
  },
  "warnings": [], "next_actions": {},
  "enrichment": {"sei": "present"},
  "meta": {"producer": {"tool": "warpline", "version": "0.1.0"},
           "local_only": true, "peer_side_effects": []}
}"#;

/// A fake warpline client that replays the recorded frozen envelope through the
/// REAL parse path (`parse_churn_count_response`) — exactly what the live MCP
/// client does after reading the subprocess response. Records the refs it was
/// asked about so the test can assert loomweave sent SEI-keyed refs.
#[derive(Default)]
struct FakeWarplineClient {
    seen_refs: Mutex<Vec<WarplineEntityRef>>,
    seen_window: Mutex<Option<Value>>,
}

impl WarplineLookup for FakeWarplineClient {
    fn entity_churn_counts(
        &self,
        entity_refs: &[WarplineEntityRef],
        window: Option<&Value>,
    ) -> Result<ChurnCountResponse, WarplineClientError> {
        *self.seen_refs.lock().unwrap() = entity_refs.to_vec();
        *self.seen_window.lock().unwrap() = window.cloned();
        // Parse the recorded frozen envelope through the production parse path.
        parse_churn_count_response(GV_LW_2_ENVELOPE).map_err(WarplineClientError::Contract)
    }
}

/// A warpline client that always errors — models unreachable / a frozen
/// `warpline.error.v1` body. The consumer must degrade to honest-unavailable.
struct UnreachableWarplineClient;

impl WarplineLookup for UnreachableWarplineClient {
    fn entity_churn_counts(
        &self,
        _entity_refs: &[WarplineEntityRef],
        _window: Option<&Value>,
    ) -> Result<ChurnCountResponse, WarplineClientError> {
        Err(WarplineClientError::WarplineError {
            tool: "warpline_entity_churn_count_get".to_owned(),
            message: "peer_unavailable".to_owned(),
        })
    }
}

/// A warpline client whose envelope carries an overflow `reason_class: "partial"`
/// — warpline truncated the read to an in-band lead. Echoes a real count for
/// alpha only; beta + gamma are absent from `items` (the truncated tail), so the
/// consumer grafts 0 onto them. The consumer must DISCLOSE this truncation
/// (`churn_truncated`) rather than letting those 0s read as never-observed.
struct PartialOverflowWarplineClient;

impl WarplineLookup for PartialOverflowWarplineClient {
    fn entity_churn_counts(
        &self,
        _entity_refs: &[WarplineEntityRef],
        _window: Option<&Value>,
    ) -> Result<ChurnCountResponse, WarplineClientError> {
        // total 3 candidates, only 1 returned in-band (the lead); reason partial.
        let envelope = format!(
            r#"{{
              "schema": "warpline.entity_churn_count.v1", "ok": true,
              "data": {{
                "items": [
                  {{"entity": {{"sei": "{SEI_ALPHA}", "locator": "{LOC_ALPHA}"}},
                   "churn_count": 7, "last_changed_at": "2026-06-13T00:00:00Z",
                   "last_actor": "agent:codex"}}
                ],
                "overflow": {{"total": 3, "returned": 1,
                             "dumped_to": "/abs/.weft/warpline/overflow/churn.json",
                             "reason_class": "partial",
                             "cause": "3 items exceeded the in-band cap",
                             "fix": "read the dump"}}
              }}
            }}"#
        );
        parse_churn_count_response(&envelope).map_err(WarplineClientError::Contract)
    }
}

/// A warpline client modelling a KEYING MISS: loomweave sends SEI refs warpline
/// has not recorded, so warpline echoes each ref with `churn_count: 0` and a
/// NULL locator. The consumer must DISCLOSE this (`churn_unresolved`) rather than
/// let the zeros read as "this code never changes" — the lacuna failure mode
/// (warpline keys by path-locator with null sei; loomweave sends dotted-locator
/// SEIs that miss).
struct UnresolvedKeyWarplineClient;

impl WarplineLookup for UnresolvedKeyWarplineClient {
    fn entity_churn_counts(
        &self,
        entity_refs: &[WarplineEntityRef],
        _window: Option<&Value>,
    ) -> Result<ChurnCountResponse, WarplineClientError> {
        // One item per ref, each a miss: sei echoed, locator null, count 0.
        let items: Vec<String> = entity_refs
            .iter()
            .map(|r| {
                format!(
                    r#"{{"entity": {{"sei": "{}", "locator": null}}, "churn_count": 0}}"#,
                    r.value
                )
            })
            .collect();
        let envelope = format!(
            r#"{{"schema": "warpline.entity_churn_count.v1", "ok": true,
                 "data": {{"items": [{}]}}}}"#,
            items.join(", ")
        );
        parse_churn_count_response(&envelope).map_err(WarplineClientError::Contract)
    }
}

fn open_project() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
    let project = tempfile::tempdir().expect("temp project");
    let dir = project.path().join(".weft/loomweave");
    std::fs::create_dir_all(&dir).expect("create .weft/loomweave");
    let db_path = dir.join("loomweave.db");
    let mut conn = Connection::open(&db_path).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    (project, db_path, conn)
}

fn insert_entity(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
            properties, content_hash, created_at, updated_at) \
         VALUES (?1,'python','function',?1,?1,'src/pkg/mod.py','{}','hash', \
                 '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![id],
    )
    .expect("insert entity");
}

fn insert_alive_sei(conn: &Connection, sei: &str, locator: &str) {
    // A run row is required by the sei_bindings.born_run_id/updated_run_id FKs.
    conn.execute(
        "INSERT OR IGNORE INTO runs (id, started_at, config, stats, status) \
         VALUES ('run-1','2026-01-01T00:00:00.000Z','{}','{}','completed')",
        [],
    )
    .expect("insert run");
    conn.execute(
        "INSERT INTO sei_bindings (sei, current_locator, body_hash, signature, status, \
            born_run_id, updated_run_id, updated_at) \
         VALUES (?1, ?2, 'bh', NULL, 'alive', 'run-1', 'run-1', '2026-01-01T00:00:00.000Z')",
        params![sei, locator],
    )
    .expect("insert sei binding");
}

/// Seed the 3 GV-LW-2 entities, each bound to its SEI.
fn seed_three_entities(conn: &Connection) {
    for (loc, sei) in [
        (LOC_ALPHA, SEI_ALPHA),
        (LOC_BETA, SEI_BETA),
        (LOC_GAMMA, SEI_GAMMA),
    ] {
        insert_entity(conn, loc);
        insert_alive_sei(conn, sei, loc);
    }
}

fn state_with_warpline(
    project_root: &std::path::Path,
    db_path: &std::path::Path,
    client: Arc<dyn WarplineLookup>,
) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool).with_warpline_client(client)
}

fn state_without_warpline(
    project_root: &std::path::Path,
    db_path: &std::path::Path,
) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
}

/// Call a tool and return the FULL success envelope
/// (`{ok, result, error, …}`). The tool payload lives under `["result"]`;
/// `["error"]` is `null` on success (a hard error sets it non-null).
async fn call_tool(state: &ServerState, name: &str, arguments: Value) -> Value {
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0", "id": "t", "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await
        .expect("tools/call returns a response");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    serde_json::from_str(text).expect("tool envelope JSON")
}

/// The tool succeeded (did not hard-error) — assert the envelope and return the
/// inner payload (`result`).
fn ok_payload(envelope: &Value) -> &Value {
    assert_eq!(
        envelope["error"],
        Value::Null,
        "tool hard-errored — the core flow must not break: {envelope}"
    );
    &envelope["result"]
}

/// GV-LW-2: `high_churn` over the 3-entity vector → all 3 echoed, two `>= 1`, the
/// never-observed one `churn_count: 0` (present, not omitted, not error),
/// ranked by count descending, count grafted onto each entity.
#[tokio::test]
async fn gv_lw_2_high_churn_ranks_three_entities() {
    let (project, db, conn) = open_project();
    seed_three_entities(&conn);
    let fake = Arc::new(FakeWarplineClient::default());
    let state = state_with_warpline(project.path(), &db, fake.clone());

    let envelope = call_tool(&state, "entity_high_churn_list", json!({})).await;
    let result = ok_payload(&envelope);
    let entities = result["entities"].as_array().expect("entities array");

    // All 3 refs are present — none omitted (the gamma=0 invariant).
    assert_eq!(entities.len(), 3, "all 3 candidates ranked, none omitted");
    assert_eq!(result["page"]["total"], json!(3));
    assert_eq!(result["churn_source"], json!("warpline"));

    // Ranked by count descending: alpha(7), beta(2), gamma(0).
    assert_eq!(entities[0]["id"], json!(LOC_ALPHA));
    assert_eq!(entities[0]["churn_count"], json!(7));
    assert_eq!(entities[1]["id"], json!(LOC_BETA));
    assert_eq!(entities[1]["churn_count"], json!(2));
    assert_eq!(entities[2]["id"], json!(LOC_GAMMA));
    assert_eq!(
        entities[2]["churn_count"],
        json!(0),
        "the never-observed entity is present with churn_count 0, not omitted, not an error"
    );

    // Two observed entities carry churn_count >= 1.
    let observed = entities
        .iter()
        .filter(|e| e["churn_count"].as_i64().unwrap_or(0) >= 1)
        .count();
    assert_eq!(observed, 2);

    // The recency fields are grafted from the frozen envelope.
    assert_eq!(
        entities[0]["last_changed_at"],
        json!("2026-06-13T00:00:00Z")
    );
    assert_eq!(entities[0]["last_actor"], json!("agent:codex"));

    // Loomweave sent SEI-keyed refs (one per candidate) — the keying contract.
    let refs = fake.seen_refs.lock().unwrap();
    assert_eq!(refs.len(), 3, "one ref per candidate, one bounded call");
    assert!(
        refs.iter().all(|r| r.kind == "sei"),
        "every candidate had a resolved SEI, so every ref is SEI-keyed"
    );
    let values: Vec<&str> = refs.iter().map(|r| r.value.as_str()).collect();
    assert!(
        values.contains(&SEI_ALPHA) && values.contains(&SEI_BETA) && values.contains(&SEI_GAMMA)
    );
}

/// `recently_changed` over the same vector → only the entities with a recorded
/// change (`churn_count >= 1`) survive, ordered by `last_changed_at` desc; the
/// `since` window is forwarded to warpline.
#[tokio::test]
async fn recently_changed_filters_unobserved_and_orders_by_last_change() {
    let (project, db, conn) = open_project();
    seed_three_entities(&conn);
    let fake = Arc::new(FakeWarplineClient::default());
    let state = state_with_warpline(project.path(), &db, fake.clone());

    let envelope = call_tool(
        &state,
        "entity_recent_change_list",
        json!({ "since": "2026-05-01T00:00:00Z" }),
    )
    .await;
    let result = ok_payload(&envelope);
    let entities = result["entities"].as_array().expect("entities array");

    // gamma (churn_count 0) is filtered out; alpha + beta remain.
    assert_eq!(
        entities.len(),
        2,
        "only entities with a recorded change remain"
    );
    // Ordered by last_changed_at desc: alpha (06-13) before beta (06-01).
    assert_eq!(entities[0]["id"], json!(LOC_ALPHA));
    assert_eq!(entities[1]["id"], json!(LOC_BETA));
    assert_eq!(result["since"], json!("2026-05-01T00:00:00Z"));
    assert_eq!(result["churn_source"], json!("warpline"));

    // The `since` was forwarded into warpline's window.
    let window = fake.seen_window.lock().unwrap();
    assert_eq!(
        window.as_ref().and_then(|w| w.get("since")),
        Some(&json!("2026-05-01T00:00:00Z"))
    );
}

/// Honest-degrade — NO warpline client wired (disabled): the surface returns
/// honest-unavailable with a warpline-named reason, NOT empty-as-clean, and does
/// NOT hard-error. (lock §1C, ENRICH-ONLY invariant.)
#[tokio::test]
async fn high_churn_degrades_honestly_when_warpline_absent() {
    let (project, db, conn) = open_project();
    seed_three_entities(&conn);
    let state = state_without_warpline(project.path(), &db);

    let envelope = call_tool(&state, "entity_high_churn_list", json!({})).await;
    // Not a hard error — the tool answered.
    let result = ok_payload(&envelope);
    // Empty, but explicitly NOT clean: a warpline-named signal carries the reason.
    assert_eq!(result["entities"].as_array().map(Vec::len), Some(0));
    assert_eq!(result["page"]["total"], json!(0));
    assert_eq!(result["churn_source"], json!("warpline"));
    assert_eq!(result["reason"], json!("warpline-disabled"));
    assert_eq!(result["signal"]["available"], json!(false));
    assert_eq!(result["signal"]["signal"], json!("warpline_churn"));
    assert!(
        result["signal"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("warpline"),
        "the missing-signal note must name warpline as the source"
    );
}

/// Honest-degrade — warpline wired but unreachable / returns an error envelope:
/// same honest-unavailable shape (`warpline-unreachable`), never empty-as-clean,
/// never a hard error. Distinguishes "warpline could not answer" from "warpline
/// answered with genuine zeros".
#[tokio::test]
async fn high_churn_degrades_honestly_when_warpline_unreachable() {
    let (project, db, conn) = open_project();
    seed_three_entities(&conn);
    let state = state_with_warpline(project.path(), &db, Arc::new(UnreachableWarplineClient));

    let envelope = call_tool(&state, "entity_high_churn_list", json!({})).await;
    let result = ok_payload(&envelope);
    assert_eq!(result["entities"].as_array().map(Vec::len), Some(0));
    assert_eq!(result["reason"], json!("warpline-unreachable"));
    assert_eq!(result["churn_source"], json!("warpline"));
    assert!(
        result["signal"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("peer_unavailable"),
        "the warpline error reason is surfaced, not swallowed"
    );
}

/// Honest-truncation — warpline answered but bounded the read to an in-band lead
/// (overflow `reason_class: "partial"`). The truncated-out entities graft
/// `churn_count: 0`, but the surface must DISCLOSE the truncation
/// (`churn_truncated`) so those 0s are not conflated with never-observed 0s. This
/// is the honesty floor for an over-cap scope (complete coverage via the overflow
/// dump is a tracked follow-up).
#[tokio::test]
async fn high_churn_discloses_warpline_overflow_truncation() {
    let (project, db, conn) = open_project();
    seed_three_entities(&conn);
    let state = state_with_warpline(project.path(), &db, Arc::new(PartialOverflowWarplineClient));

    let envelope = call_tool(&state, "entity_high_churn_list", json!({})).await;
    let result = ok_payload(&envelope);

    // Not a hard error; the in-band entity carries its real count.
    assert_eq!(result["churn_source"], json!("warpline"));
    let alpha = result["entities"]
        .as_array()
        .expect("entities")
        .iter()
        .find(|e| e["id"] == json!(LOC_ALPHA))
        .expect("alpha present");
    assert_eq!(alpha["churn_count"], json!(7));

    // The truncation is disclosed with warpline's own counts — NOT silently
    // swallowed into all-plausible zeros.
    let truncated = &result["churn_truncated"];
    assert_eq!(truncated["truncated"], json!(true));
    assert_eq!(truncated["total_candidates"], json!(3));
    assert_eq!(truncated["counted"], json!(1));
    assert_eq!(truncated["uncounted"], json!(2));
    assert!(
        truncated["reason"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("truncat"),
        "the disclosure must name the truncation: {truncated}"
    );
}

/// Honest keying-miss — warpline answered but could not key-match the refs (null
/// locator, count 0). A non-empty all-zero result must DISCLOSE the keying gap
/// (`churn_unresolved`) so the zeros are not read as "never changes" — the
/// scoped-all-zeros failure the overflow disclosure's twin must also cover.
#[tokio::test]
async fn high_churn_discloses_warpline_keying_miss() {
    let (project, db, conn) = open_project();
    seed_three_entities(&conn);
    let state = state_with_warpline(project.path(), &db, Arc::new(UnresolvedKeyWarplineClient));

    let envelope = call_tool(&state, "entity_high_churn_list", json!({})).await;
    let result = ok_payload(&envelope);

    // Non-empty (3 candidates ranked) but every count is 0 — and that is DISCLOSED
    // as a keying miss, not silently shipped as a clean all-zero answer.
    assert_eq!(result["page"]["total"], json!(3));
    assert_eq!(result["churn_source"], json!("warpline"));
    assert_eq!(result["churn_unresolved"]["count"], json!(3));
    assert!(
        result["churn_unresolved"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("key"),
        "the disclosure must name the keying gap: {}",
        result["churn_unresolved"]
    );
    // A genuine all-zero (resolved, never-observed) must NOT trip this — covered by
    // the GV-LW-2 fixture where every item has a non-null locator (unresolved 0).
}
