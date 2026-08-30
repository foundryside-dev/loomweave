use loomweave_mcp::ServerState;
use loomweave_storage::{ReaderPool, pragma, schema};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const GOLDEN: &[u8] =
    include_bytes!("../../../docs/federation/fixtures/classification.python.json");
const GOLDEN_SHA256: &str =
    include_str!("../../../docs/federation/fixtures/classification.python.json.sha256");

#[test]
fn real_python_classification_golden_is_supported_complete_5_5_0_0_and_byte_pinned() {
    let fixture: serde_json::Value = serde_json::from_slice(GOLDEN).expect("parse golden");
    for (tag, matches) in [
        ("cli-command", 5),
        ("entry-point", 5),
        ("http-route", 0),
        ("exported-api", 0),
    ] {
        let envelope = &fixture["responses"][tag];
        assert_eq!(envelope["ok"], true, "{tag}");
        assert_eq!(
            envelope["result"]["classification"]["state"], "supported",
            "{tag}"
        );
        assert_eq!(
            envelope["result"]["classification"]["complete"], true,
            "{tag}"
        );
        assert_eq!(
            envelope["result"]["classification"]["matches"], matches,
            "{tag}"
        );
        assert_eq!(envelope["result"]["page"]["total"], matches, "{tag}");
        assert_eq!(envelope["result"]["signal"]["available"], true, "{tag}");
        assert_eq!(envelope["result"]["signal"]["complete"], true, "{tag}");
        for entity in envelope["result"]["entities"].as_array().expect("entities") {
            let sei = entity["sei"].as_str().expect("normalized SEI token");
            assert!(sei.starts_with("normalized-sei:"), "{tag}: {sei}");
            assert!(
                !loomweave_storage::is_reserved_sei(sei),
                "normalization placeholder must not masquerade as a production SEI: {sei}"
            );
        }
    }
    assert!(
        fixture["normalization"]
            .as_array()
            .expect("normalization rules")
            .iter()
            .filter_map(Value::as_str)
            .any(|rule| rule.contains("not production mints"))
    );

    let expected = GOLDEN_SHA256
        .split_whitespace()
        .next()
        .expect("digest sidecar");
    assert_eq!(format!("{:x}", Sha256::digest(GOLDEN)), expected);
    let mut mutated = GOLDEN.to_vec();
    mutated[0] ^= 1;
    assert_ne!(format!("{:x}", Sha256::digest(mutated)), expected);
}

fn normalize_root(value: &mut Value, root: &str) {
    match value {
        Value::Object(object) => {
            for item in object.values_mut() {
                normalize_root(item, root);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_root(item, root);
            }
        }
        Value::String(text) if text.starts_with(root) => {
            *text = format!("<fixture-root>{}", &text[root.len()..]);
        }
        _ => {}
    }
}

async fn call_tag(state: &ServerState, tag: &str) -> Value {
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "golden",
            "method": "tools/call",
            "params": {
                "name": "entity_tag_list",
                "arguments": {"tag": tag, "limit": 200}
            }
        }))
        .await
        .expect("production tag handler response");
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("MCP text envelope"),
    )
    .expect("tag envelope JSON")
}

#[tokio::test]
async fn production_tag_handler_exactly_reproduces_all_four_normalized_goldens() {
    let fixture: Value = serde_json::from_slice(GOLDEN).expect("parse classification golden");
    let coverage =
        include_str!("../../loomweave-storage/tests/fixtures/classifier-coverage-v1.golden.json");
    let project = tempfile::tempdir().expect("temporary project");
    let store = project.path().join(".weft/loomweave");
    std::fs::create_dir_all(&store).expect("create store");
    let db_path = store.join("loomweave.db");
    let mut conn = Connection::open(&db_path).expect("open catalogue");
    pragma::apply_write_pragmas(&conn).expect("apply write pragmas");
    schema::apply_migrations(&mut conn).expect("migrate catalogue");
    let run_stats = format!("{{\"classifier_coverage\":{coverage}}}");
    conn.execute(
        "INSERT INTO runs(id, started_at, completed_at, config, stats, status) \
         VALUES ('<run-id>', '2026-07-12T00:00:00Z', '2026-07-12T00:00:01Z', '{}', ?1, 'completed')",
        params![run_stats],
    )
    .expect("insert exact producer coverage");

    // Slim list rows (X-6, clarion-b24df21158) carry only
    // {id, sei, kind, short_name, source_file_path, source_line_start}, so the
    // reconstruction (a) derives tag membership from WHICH per-tag responses an
    // entity appears in, and (b) seeds synthetic values for the columns the
    // response no longer echoes (name / line_end / content_hash) — they cannot
    // influence the reproduced bytes, which is the point of the slimming.
    let mut rows: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut tags_by_id: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for tag in ["cli-command", "entry-point", "http-route", "exported-api"] {
        for entity in fixture["responses"][tag]["result"]["entities"]
            .as_array()
            .expect("golden entities")
        {
            let id = entity["id"].as_str().expect("entity id").to_owned();
            tags_by_id
                .entry(id.clone())
                .or_default()
                .insert(tag.to_owned());
            rows.entry(id).or_insert_with(|| entity.clone());
        }
    }
    // Tags the index holds beyond the four queried (e.g. `public-surface`)
    // surface only through the empty responses' `known_tags`; per-entity
    // membership is invisible to the slim rows and irrelevant to the bytes —
    // pin each such tag to the first entity so `known_tags` reproduces.
    let first_id = rows.keys().next().expect("at least one entity").clone();
    for tag in ["cli-command", "entry-point", "http-route", "exported-api"] {
        let Some(known) = fixture["responses"][tag]["result"]["known_tags"].as_array() else {
            continue;
        };
        for known_tag in known.iter().filter_map(Value::as_str) {
            tags_by_id
                .entry(first_id.clone())
                .or_default()
                .insert(known_tag.to_owned());
        }
    }
    for (id, entity) in &rows {
        let relative = entity["source_file_path"].as_str().expect("source path");
        let relative = relative.strip_prefix("<fixture-root>/").unwrap_or(relative);
        let source_path = project.path().join(relative);
        std::fs::write(&source_path, "# production handler golden\n").expect("seed source");
        let qualname = id.rsplit(':').next().expect("qualname");
        let synthetic_hash = format!("golden-hash-{qualname}");
        let line_start = entity["source_line_start"].as_i64().expect("start line");
        conn.execute(
            "INSERT INTO entities( \
                 id, plugin_id, kind, name, short_name, source_file_path, \
                 source_line_start, source_line_end, properties, content_hash, created_at, updated_at \
             ) VALUES (?1, 'python', ?2, ?3, ?4, ?5, ?6, ?7, '{}', ?8, \
                       '2026-07-12T00:00:00Z', '2026-07-12T00:00:00Z')",
            params![
                id,
                entity["kind"].as_str().expect("kind"),
                qualname,
                entity["short_name"].as_str().expect("short name"),
                source_path.to_string_lossy(),
                line_start,
                line_start + 1,
                synthetic_hash,
            ],
        )
        .expect("insert golden entity");
        for tag in &tags_by_id[id] {
            conn.execute(
                "INSERT INTO entity_tags(entity_id, plugin_id, tag) VALUES (?1, 'python', ?2)",
                params![id, tag],
            )
            .expect("insert golden tag");
        }
        conn.execute(
            "INSERT INTO sei_bindings( \
                 sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at \
             ) VALUES (?1, ?2, ?3, NULL, 'alive', '<run-id>', '<run-id>', '2026-07-12T00:00:01Z')",
            params![entity["sei"].as_str().expect("SEI"), id, synthetic_hash],
        )
        .expect("insert golden SEI");
    }
    drop(conn);

    let state = ServerState::new(
        project.path().to_path_buf(),
        ReaderPool::open(&db_path, 2).expect("reader pool"),
    );
    let root = project.path().to_string_lossy();
    for tag in ["cli-command", "entry-point", "http-route", "exported-api"] {
        let mut actual = call_tag(&state, tag).await;
        normalize_root(&mut actual, &root);
        assert_eq!(actual, fixture["responses"][tag], "{tag}");
    }
}
