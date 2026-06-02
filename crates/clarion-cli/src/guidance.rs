//! `clarion guidance` authoring subcommands (WS6 / REQ-GUIDANCE-03).
//!
//! Operator-facing CLI to create, edit, show, list, and delete guidance sheets
//! — the institutional-knowledge entities (`kind = 'guidance'`) the MCP read
//! path composes into briefings. All SQL lives in `clarion-storage`
//! (`clarion_storage::guidance`); this module owns only argument parsing, the
//! `$EDITOR` round-trip, and presentation.
//!
//! `promote` (consume a Filigree observation into a sheet) is intentionally NOT
//! here — it depends on the observation lifecycle owned by a later WS6 task.
//!
//! ## `--match` syntax
//!
//! Each `--match` value is `<type>:<value>` (split on the **first** colon only,
//! because subsystem/entity values themselves contain colons):
//!   - `path:<glob>`            → `{"type":"path","pattern":"<glob>"}`
//!   - `tag:<tag>`              → `{"type":"tag","value":"<tag>"}`
//!   - `kind:<entity-kind>`     → `{"type":"kind","value":"<entity-kind>"}`
//!   - `subsystem:<id>`         → `{"type":"subsystem","id":"<id>"}`
//!   - `entity:<entity-id>`     → `{"type":"entity","id":"<entity-id>"}`
//!
//! e.g. `--match path:src/auth/** --match subsystem:core:subsystem:abcd
//! --match entity:python:function:foo.bar`. The emitted objects are exactly the
//! shape the read path's `rule_match` consumes.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

use clarion_storage::{
    GuidanceSheet, GuidanceSheetInput, delete_guidance_sheet, get_guidance_sheet,
    guidance_sheet_matches_entity, list_guidance_sheets, upsert_guidance_sheet,
};

use crate::cli::GuidanceCommand;

/// Map a `clarion_storage::StorageError` (which is `Send` but not `Sync`, so it
/// does not satisfy `anyhow`'s `From` bound) into an `anyhow::Error` via its
/// Display — matching the convention in `analyze.rs`.
trait StorageResultExt<T> {
    fn into_anyhow(self) -> Result<T>;
}

impl<T> StorageResultExt<T> for clarion_storage::Result<T> {
    fn into_anyhow(self) -> Result<T> {
        self.map_err(|e| anyhow!("{e}"))
    }
}

/// The canonical scope-level vocabulary (ADR-024). Ordered project→function so
/// the message lists them in rank order.
const SCOPE_LEVELS: &[&str] = &[
    "project",
    "subsystem",
    "package",
    "module",
    "class",
    "function",
];

const PROVENANCE_MANUAL: &str = "manual";

/// Dispatch a `clarion guidance <subcommand>`.
///
/// # Errors
///
/// Surfaces parse errors (bad `--match` / `--scope-level`), I/O errors
/// (`$EDITOR`, stdin), and storage errors. Not-found on `show`/`edit`/`delete`
/// is a clean non-panicking error.
pub fn run(command: GuidanceCommand) -> Result<()> {
    match command {
        GuidanceCommand::Create {
            path,
            r#match,
            scope_level,
            content,
            name,
            pinned,
            expires,
        } => create(
            &path,
            &r#match,
            &scope_level,
            content,
            name.as_deref(),
            pinned,
            expires.as_deref(),
        ),
        GuidanceCommand::Edit { path, id } => edit(&path, &id),
        GuidanceCommand::Show { path, id } => show(&path, &id),
        GuidanceCommand::List { path, for_entity } => list(&path, for_entity.as_deref()),
        GuidanceCommand::Delete { path, id } => delete(&path, &id),
    }
}

// ── Match-rule parsing (TDD target #1) ────────────────────────────────────────

/// Parse one `--match` value into its `match_rules` JSON object. Splits on the
/// first colon only; the value half is opaque (subsystem/entity ids contain
/// colons).
///
/// # Errors
///
/// Errors on a missing colon, an empty value, or an unknown rule type.
fn parse_match_rule(raw: &str) -> Result<Value> {
    let (rule_type, value) = raw
        .split_once(':')
        .ok_or_else(|| anyhow!("--match '{raw}': expected '<type>:<value>' (e.g. path:src/**)"))?;
    if value.is_empty() {
        bail!("--match '{raw}': empty value after '{rule_type}:'");
    }
    let rule = match rule_type {
        "path" => json!({ "type": "path", "pattern": value }),
        "tag" => json!({ "type": "tag", "value": value }),
        "kind" => json!({ "type": "kind", "value": value }),
        "subsystem" => json!({ "type": "subsystem", "id": value }),
        "entity" => json!({ "type": "entity", "id": value }),
        other => bail!(
            "--match '{raw}': unknown rule type '{other}' \
             (expected one of: path, tag, kind, subsystem, entity)"
        ),
    };
    Ok(rule)
}

/// Parse all `--match` values into the `match_rules` array.
fn parse_match_rules(raw: &[String]) -> Result<Vec<Value>> {
    raw.iter().map(|r| parse_match_rule(r)).collect()
}

fn validate_scope_level(level: &str) -> Result<()> {
    if SCOPE_LEVELS.contains(&level) {
        Ok(())
    } else {
        bail!(
            "--scope-level '{level}' is not valid (expected one of: {})",
            SCOPE_LEVELS.join(", ")
        )
    }
}

/// Derive a canonical slug for the entity id's third segment from `--name` (or,
/// when absent, the first match rule). The slug must satisfy the canonical-name
/// grammar; we keep alphanumerics, dot, hyphen, underscore and replace any other
/// run with a single hyphen.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        // Fall back to a timestamp-ish token so the id is always well-formed.
        format!(
            "sheet-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        )
    } else {
        trimmed
    }
}

// ── Subcommand handlers ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn create(
    project_root: &Path,
    raw_match: &[String],
    scope_level: &str,
    content: Option<String>,
    name: Option<&str>,
    pinned: bool,
    expires: Option<&str>,
) -> Result<()> {
    validate_scope_level(scope_level)?;
    let match_rules = parse_match_rules(raw_match)?;

    // Content: explicit flag, else stdin / $EDITOR.
    let content = match content {
        Some(text) => text,
        None => read_content_interactively("")?,
    };
    if content.trim().is_empty() {
        bail!("guidance content is empty; pass --content or provide text in the editor");
    }

    let slug_source = name.unwrap_or_else(|| raw_match.first().map_or("guidance", String::as_str));
    let slug = slugify(slug_source);
    let id = format!("core:guidance:{slug}");
    let short_name = slug.rsplit('.').next().unwrap_or(&slug).to_owned();

    let conn = open_db(project_root)?;
    if get_guidance_sheet(&conn, &id).into_anyhow()?.is_some() {
        bail!("guidance sheet {id} already exists; use `clarion guidance edit {id}`");
    }

    let now = now_iso8601(&conn)?;
    let mut properties = json!({
        "content": content,
        "scope_level": scope_level,
        "match_rules": match_rules,
        "pinned": pinned,
        "provenance": PROVENANCE_MANUAL,
        "authored_at": now,
    });
    if let Some(expires) = expires
        && let Some(obj) = properties.as_object_mut()
    {
        obj.insert("expires".to_owned(), json!(expires));
    }

    upsert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: &id,
            name: &slug,
            short_name: &short_name,
            properties: &properties,
        },
    )
    .into_anyhow()
    .context("write guidance sheet")?;
    // TODO(T-cache): invalidate dependent summaries when a new sheet changes
    // which entities have applicable guidance.

    println!("Created guidance sheet {id}");
    Ok(())
}

fn edit(project_root: &Path, id: &str) -> Result<()> {
    let conn = open_db(project_root)?;
    let sheet = get_guidance_sheet(&conn, id)
        .into_anyhow()?
        .ok_or_else(|| anyhow!("guidance sheet {id} not found"))?;

    let current = sheet
        .properties
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let new_content = edit_in_editor(current)?;
    if new_content.trim().is_empty() {
        bail!("guidance content is empty after edit; aborting (sheet unchanged)");
    }
    if new_content == current {
        println!("No changes to guidance sheet {id}");
        return Ok(());
    }

    // Read-modify-write: preserve every existing property (authored_at,
    // provenance, pinned, expires, scope_level, match_rules, …) and replace
    // only `content`. Edit must NOT regenerate authored_at (the staleness
    // baseline) or flip provenance.
    let mut properties = sheet.properties.clone();
    if let Some(obj) = properties.as_object_mut() {
        obj.insert("content".to_owned(), json!(new_content));
    } else {
        bail!("guidance sheet {id} has malformed properties; cannot edit");
    }

    upsert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: &sheet.id,
            name: &sheet.name,
            short_name: &sheet.short_name,
            properties: &properties,
        },
    )
    .into_anyhow()
    .context("write edited guidance sheet")?;
    // TODO(T-cache): invalidate dependent summaries on content change.

    println!("Updated guidance sheet {id}");
    Ok(())
}

fn show(project_root: &Path, id: &str) -> Result<()> {
    let conn = open_db(project_root)?;
    let sheet = get_guidance_sheet(&conn, id)
        .into_anyhow()?
        .ok_or_else(|| anyhow!("guidance sheet {id} not found"))?;
    print!("{}", render_sheet(&sheet));
    Ok(())
}

fn list(project_root: &Path, for_entity: Option<&str>) -> Result<()> {
    let conn = open_db(project_root)?;
    let sheets = list_guidance_sheets(&conn).into_anyhow()?;

    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let mut shown = 0usize;
    for sheet in &sheets {
        if let Some(entity_id) = for_entity
            && !guidance_sheet_matches_entity(&conn, sheet, entity_id, &canonical_root)
                .into_anyhow()?
        {
            continue;
        }
        println!("{}", render_sheet_line(sheet));
        shown += 1;
    }
    if shown == 0 {
        match for_entity {
            Some(entity_id) => println!("(no guidance sheets match {entity_id})"),
            None => println!("(no guidance sheets)"),
        }
    }
    Ok(())
}

fn delete(project_root: &Path, id: &str) -> Result<()> {
    let conn = open_db(project_root)?;
    if delete_guidance_sheet(&conn, id).into_anyhow()? {
        // TODO(T-cache): invalidate dependent summaries for entities this sheet
        // matched.
        println!("Deleted guidance sheet {id}");
        Ok(())
    } else {
        bail!("guidance sheet {id} not found")
    }
}

// ── Presentation ──────────────────────────────────────────────────────────────

fn render_sheet_line(sheet: &GuidanceSheet) -> String {
    let level = sheet.scope_level.as_deref().unwrap_or("?");
    let pinned = sheet
        .properties
        .get("pinned")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pin = if pinned { " [pinned]" } else { "" };
    let rules = sheet
        .properties
        .get("match_rules")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    format!("{}  ({level}, {rules} rule(s)){pin}", sheet.id)
}

fn render_sheet(sheet: &GuidanceSheet) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "id:          {}", sheet.id);
    let _ = writeln!(
        out,
        "scope_level: {}",
        sheet.scope_level.as_deref().unwrap_or("?")
    );
    let field = |key: &str| -> Option<String> {
        sheet.properties.get(key).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        })
    };
    if let Some(p) = field("provenance") {
        let _ = writeln!(out, "provenance:  {p}");
    }
    if let Some(p) = field("pinned") {
        let _ = writeln!(out, "pinned:      {p}");
    }
    if let Some(a) = field("authored_at") {
        let _ = writeln!(out, "authored_at: {a}");
    }
    if let Some(e) = field("expires") {
        let _ = writeln!(out, "expires:     {e}");
    }
    if let Some(rules) = sheet
        .properties
        .get("match_rules")
        .and_then(Value::as_array)
    {
        out.push_str("match_rules:\n");
        for rule in rules {
            let _ = writeln!(out, "  - {rule}");
        }
    }
    out.push_str("content:\n");
    let content = sheet
        .properties
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("");
    for line in content.lines() {
        let _ = writeln!(out, "  {line}");
    }
    out
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Open a read-write connection to `.clarion/clarion.db` with a generous busy
/// timeout so a concurrently-running `serve` writer does not cause an immediate
/// lock error.
fn open_db(project_root: &Path) -> Result<Connection> {
    let db_path = project_root.join(".clarion").join("clarion.db");
    if !db_path.exists() {
        bail!(
            "Clarion database not found at {}; run `clarion analyze` first",
            db_path.display()
        );
    }
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open database {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("set busy_timeout")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("enable foreign_keys")?;
    Ok(conn)
}

/// Read guidance content from stdin if it is piped, otherwise launch `$EDITOR`.
fn read_content_interactively(seed: &str) -> Result<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read guidance content from stdin")?;
        return Ok(buf);
    }
    edit_in_editor(seed)
}

/// Launch `$EDITOR` (or `$VISUAL`) on a temp file seeded with `seed` and return
/// the saved contents.
fn edit_in_editor(seed: &str) -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| anyhow!("neither $VISUAL nor $EDITOR is set; set one or pass --content"))?;

    let dir = std::env::temp_dir();
    let file = dir.join(format!("clarion-guidance-{}.md", std::process::id()));
    {
        let mut f = std::fs::File::create(&file)
            .with_context(|| format!("create temp edit file {}", file.display()))?;
        f.write_all(seed.as_bytes())
            .context("seed temp edit file")?;
    }

    let status = run_editor(&editor, &file)?;
    let result = if status {
        std::fs::read_to_string(&file).context("read back edited content")
    } else {
        Err(anyhow!("editor '{editor}' exited with a non-zero status"))
    };
    let _ = std::fs::remove_file(&file);
    result
}

/// Run the editor command (which may carry arguments, e.g. `code --wait`) on
/// `file`. Returns whether it exited successfully.
fn run_editor(editor: &str, file: &Path) -> Result<bool> {
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("$EDITOR/$VISUAL is empty"))?;
    let args: Vec<&str> = parts.collect();
    let status = std::process::Command::new(program)
        .args(&args)
        .arg(file)
        .status()
        .with_context(|| format!("launch editor '{editor}'"))?;
    Ok(status.success())
}

/// Mint the sheet's `authored_at` using the open connection's own clock, in the
/// exact `strftime('%Y-%m-%dT%H:%M:%fZ','now')` shape the storage layer stamps
/// `created_at` / `updated_at` with — so `authored_at` sorts lexically
/// alongside stored timestamps with zero formatting drift. It is a distinct
/// property: `created_at`/`updated_at` move on every write, `authored_at` is set
/// once and preserved across `edit` (the staleness baseline T5 reads).
fn now_iso8601(conn: &Connection) -> Result<String> {
    let ts: String = conn
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
            row.get(0)
        })
        .context("mint authored_at timestamp")?;
    Ok(ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_path_rule() {
        assert_eq!(
            parse_match_rule("path:src/auth/**").unwrap(),
            json!({"type": "path", "pattern": "src/auth/**"})
        );
    }

    #[test]
    fn parse_tag_rule() {
        assert_eq!(
            parse_match_rule("tag:auth").unwrap(),
            json!({"type": "tag", "value": "auth"})
        );
    }

    #[test]
    fn parse_kind_rule() {
        assert_eq!(
            parse_match_rule("kind:function").unwrap(),
            json!({"type": "kind", "value": "function"})
        );
    }

    #[test]
    fn parse_subsystem_rule_keeps_colons_in_value() {
        // The value half is opaque and itself contains colons — split once only.
        assert_eq!(
            parse_match_rule("subsystem:core:subsystem:abcd").unwrap(),
            json!({"type": "subsystem", "id": "core:subsystem:abcd"})
        );
    }

    #[test]
    fn parse_entity_rule_keeps_colons_in_value() {
        assert_eq!(
            parse_match_rule("entity:python:function:foo.bar").unwrap(),
            json!({"type": "entity", "id": "python:function:foo.bar"})
        );
    }

    #[test]
    fn parse_path_glob_with_no_extra_colons() {
        assert_eq!(
            parse_match_rule("path:**/refresh.py").unwrap(),
            json!({"type": "path", "pattern": "**/refresh.py"})
        );
    }

    #[test]
    fn parse_rejects_missing_colon() {
        let err = parse_match_rule("pathsrc").unwrap_err().to_string();
        assert!(err.contains("expected '<type>:<value>'"), "{err}");
    }

    #[test]
    fn parse_rejects_empty_value() {
        let err = parse_match_rule("tag:").unwrap_err().to_string();
        assert!(err.contains("empty value"), "{err}");
    }

    #[test]
    fn parse_rejects_unknown_type() {
        let err = parse_match_rule("colour:blue").unwrap_err().to_string();
        assert!(err.contains("unknown rule type 'colour'"), "{err}");
    }

    #[test]
    fn parse_many_collects_all() {
        let raw = vec![
            "path:src/**".to_owned(),
            "tag:auth".to_owned(),
            "entity:python:function:x.y".to_owned(),
        ];
        let rules = parse_match_rules(&raw).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(
            rules[2],
            json!({"type": "entity", "id": "python:function:x.y"})
        );
    }

    #[test]
    fn parse_many_propagates_first_error() {
        let raw = vec!["path:ok".to_owned(), "bad".to_owned()];
        assert!(parse_match_rules(&raw).is_err());
    }

    #[test]
    fn scope_level_validation() {
        assert!(validate_scope_level("module").is_ok());
        assert!(validate_scope_level("project").is_ok());
        assert!(validate_scope_level("subsystem").is_ok());
        assert!(validate_scope_level("nonsense").is_err());
    }

    #[test]
    fn slugify_cleans_unsafe_chars() {
        assert_eq!(slugify("auth tokens"), "auth-tokens");
        assert_eq!(slugify("pkg.mod.fn"), "pkg.mod.fn");
        assert_eq!(slugify("path:src/**"), "path-src");
        assert_eq!(slugify("a__b-c.d"), "a__b-c.d");
    }

    #[test]
    fn now_iso8601_is_well_formed() {
        let conn = Connection::open_in_memory().unwrap();
        let ts = now_iso8601(&conn).unwrap();
        // YYYY-MM-DDTHH:MM:SS.mmmZ — 24 chars, sorts lexically.
        assert_eq!(ts.len(), 24, "{ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
