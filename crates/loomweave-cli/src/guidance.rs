//! `loomweave guidance` authoring subcommands (WS6 / REQ-GUIDANCE-03).
//!
//! Operator-facing CLI to create, edit, show, list, and delete guidance sheets
//! — the institutional-knowledge entities (`kind = 'guidance'`) the MCP read
//! path composes into briefings. All SQL lives in `loomweave-storage`
//! (`loomweave_storage::guidance`); this module owns only argument parsing, the
//! `$EDITOR` round-trip, and presentation.
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

use loomweave_federation::filigree::{FiligreeHttpClient, FiligreeLookup};
use loomweave_storage::{
    GuidanceProposal, GuidanceSheet, GuidanceSheetInput, PortableSheet, delete_guidance_sheet,
    get_guidance_sheet, guidance_sheet_is_expired, guidance_sheet_is_stale,
    guidance_sheet_matches_entity, import_portable_sheet, insert_guidance_sheet,
    invalidate_summaries_for_sheet, list_guidance_sheets, upsert_guidance_sheet,
};

use crate::cli::GuidanceCommand;

/// Map a `loomweave_storage::StorageError` (which is `Send` but not `Sync`, so it
/// does not satisfy `anyhow`'s `From` bound) into an `anyhow::Error` via its
/// Display — matching the convention in `analyze.rs`.
trait StorageResultExt<T> {
    fn into_anyhow(self) -> Result<T>;
}

impl<T> StorageResultExt<T> for loomweave_storage::Result<T> {
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

/// Dispatch a `loomweave guidance <subcommand>`.
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
            CreateArgs {
                raw_match: &r#match,
                scope_level: &scope_level,
                content,
                name: name.as_deref(),
                pinned,
                expires: expires.as_deref(),
            },
        ),
        GuidanceCommand::Edit { path, id } => edit(&path, &id),
        GuidanceCommand::Show { path, id } => show(&path, &id),
        GuidanceCommand::List {
            path,
            for_entity,
            expired,
            stale,
            days,
        } => list(
            &path,
            ListFilters {
                for_entity: for_entity.as_deref(),
                expired,
                stale,
                days,
            },
        ),
        GuidanceCommand::Delete { path, id } => delete(&path, &id),
        GuidanceCommand::Promote {
            path,
            config,
            observation_id,
        } => promote(&path, config.as_deref(), &observation_id),
        GuidanceCommand::Export { path, to } => export(&path, &to),
        GuidanceCommand::Import { path, dir } => import(&path, &dir),
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

/// Inputs for `create`, grouped so the handler takes one struct instead of a
/// long positional argument list.
struct CreateArgs<'a> {
    raw_match: &'a [String],
    scope_level: &'a str,
    content: Option<String>,
    name: Option<&'a str>,
    pinned: bool,
    expires: Option<&'a str>,
}

fn create(project_root: &Path, args: CreateArgs<'_>) -> Result<()> {
    validate_scope_level(args.scope_level)?;
    let match_rules = parse_match_rules(args.raw_match)?;

    // Content: explicit flag, else stdin / $EDITOR.
    let content = match args.content {
        Some(text) => text,
        None => read_content_interactively("")?,
    };
    if content.trim().is_empty() {
        bail!("guidance content is empty; pass --content or provide text in the editor");
    }

    let slug_source = args
        .name
        .unwrap_or_else(|| args.raw_match.first().map_or("guidance", String::as_str));
    let slug = slugify(slug_source);
    let id = format!("core:guidance:{slug}");
    let short_name = slug.rsplit('.').next().unwrap_or(&slug).to_owned();

    let conn = open_db(project_root)?;

    // Normalise `--expires` *before* the write so the stored instant is
    // byte-format-identical to the read path's `now` (the expiry compare is
    // lexical, so a raw date-only or offset string would mis-order). Reject
    // unparseable input up front, mirroring `validate_scope_level`.
    let expires = args
        .expires
        .map(|raw| normalize_expires(&conn, raw))
        .transpose()?;

    let now = now_iso8601(&conn)?;
    let mut properties = json!({
        "content": content,
        "scope_level": args.scope_level,
        "match_rules": match_rules,
        "pinned": args.pinned,
        "provenance": PROVENANCE_MANUAL,
        "authored_at": now,
    });
    if let Some(expires) = expires
        && let Some(obj) = properties.as_object_mut()
    {
        obj.insert("expires".to_owned(), json!(expires));
    }

    insert_guidance_sheet(
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

    // ADR-007 churn-eager invalidation: a new sheet adds guidance to the
    // entities its match_rules cover, so their cached summaries must be dropped
    // or the guidance stays inert until each entity's code changes. Re-fetch the
    // just-written sheet (cleaner than hand-rolling a `GuidanceSheet`) and
    // invalidate the entities it matches.
    //
    // Non-atomic: the sheet write is already committed above; an error here can
    // leave a committed sheet alongside a stale cache row. Self-healing — a
    // re-run, or the next cache-key rotation when the entity's code changes,
    // clears it. Over-invalidation is safe; under-invalidation is the only bug.
    let invalidated = invalidate_matched_summaries(project_root, &conn, &id)?;

    println!("Created guidance sheet {id}");
    report_invalidation(invalidated);
    Ok(())
}

/// Invalidate cached summaries for every entity the sheet `id` matches, using
/// the canonicalized project root the storage matcher needs for `path:` rules.
/// Re-fetches the sheet by id so callers don't hand-build a `GuidanceSheet`.
/// A missing sheet (e.g. raced away) is a clean 0.
fn invalidate_matched_summaries(project_root: &Path, conn: &Connection, id: &str) -> Result<usize> {
    let Some(sheet) = get_guidance_sheet(conn, id).into_anyhow()? else {
        return Ok(0);
    };
    invalidate_summaries_for_sheet(conn, &sheet, project_root).into_anyhow()
}

/// Print a short operator note when summaries were invalidated. Silent on 0 so
/// the common no-match case stays quiet.
fn report_invalidation(count: usize) {
    if count > 0 {
        let plural = if count == 1 { "summary" } else { "summaries" };
        println!("Invalidated {count} cached {plural}");
    }
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

    // ADR-007 churn-eager invalidation: the edit changed `content`, so the
    // composed guidance for every matched entity changed and their cached
    // summaries are stale. Invalidate the union of entities matched before and
    // after the edit. `edit` only mutates `content` (match_rules are preserved),
    // so before == after today — but compute the union defensively so a future
    // rule-editing path stays correct without a second visit here. The earlier
    // `sheet` snapshot carries the pre-edit rules; `id` re-fetches the post-edit
    // sheet.
    //
    // Non-atomic: the edited sheet is already committed above; an error here can
    // leave it alongside a stale cache row. Self-healing on re-run / next
    // cache-key rotation (same posture as `create`).
    let invalidated = invalidate_matched_summaries_union(project_root, &conn, &sheet, id)?;

    println!("Updated guidance sheet {id}");
    report_invalidation(invalidated);
    Ok(())
}

/// Invalidate the union of entities matched by `before` (a pre-edit snapshot)
/// and by the sheet currently stored under `id`. The returned count is the true
/// number of rows removed across the union, with no double-count: pass 1
/// (`before`) deletes its matched rows, which removes those entities from pass 2's
/// driving `SELECT DISTINCT entity_id FROM summary_cache`, so pass 2 never
/// re-tests an already-cleared entity — only after-only entities remain for it
/// to delete.
fn invalidate_matched_summaries_union(
    project_root: &Path,
    conn: &Connection,
    before: &GuidanceSheet,
    id: &str,
) -> Result<usize> {
    let mut removed = invalidate_summaries_for_sheet(conn, before, project_root).into_anyhow()?;
    removed += invalidate_matched_summaries(project_root, conn, id)?;
    Ok(removed)
}

fn show(project_root: &Path, id: &str) -> Result<()> {
    let conn = open_db(project_root)?;
    let sheet = get_guidance_sheet(&conn, id)
        .into_anyhow()?
        .ok_or_else(|| anyhow!("guidance sheet {id} not found"))?;
    print!("{}", render_sheet(&sheet));
    Ok(())
}

/// Filters for `loomweave guidance list`. All active filters compose by
/// **intersection** (AND): a sheet is shown only if it passes every one. The
/// two date filters (`expired`, `stale`) are independent — combining them shows
/// sheets that are expired AND stale, which is the intuitive "show me the worst
/// of the worst" operator-triage reading and falls out of the simplest code.
#[derive(Clone, Copy)]
struct ListFilters<'a> {
    for_entity: Option<&'a str>,
    expired: bool,
    stale: bool,
    /// Staleness window in days (used only when `stale` is set).
    days: u32,
}

fn list(project_root: &Path, filters: ListFilters<'_>) -> Result<()> {
    let conn = open_db(project_root)?;
    let sheets = list_guidance_sheets(&conn).into_anyhow()?;

    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    // Compute the comparison instants once, from the connection's own clock, in
    // the exact `YYYY-MM-DDTHH:MM:SS.mmmZ` shape stored timestamps use — so the
    // storage predicates' lexical compares are valid instant compares. `now`
    // drives `--expired`; `stale_before` (now − N days) drives `--stale`. NB:
    // `--stale` here is the *age/review-cadence* signal (system-design §7.741),
    // distinct from the churn-based `LMWV-FACT-GUIDANCE-CHURN-STALE` finding.
    let now = now_iso8601(&conn)?;
    let stale_before = if filters.stale {
        Some(now_minus_days(&conn, filters.days)?)
    } else {
        None
    };

    let mut shown = 0usize;
    for sheet in &sheets {
        if let Some(entity_id) = filters.for_entity
            && !guidance_sheet_matches_entity(&conn, sheet, entity_id, &canonical_root)
                .into_anyhow()?
        {
            continue;
        }
        if filters.expired && !guidance_sheet_is_expired(sheet, &now) {
            continue;
        }
        if let Some(cutoff) = stale_before.as_deref()
            && !guidance_sheet_is_stale(sheet, cutoff)
        {
            continue;
        }
        println!("{}", render_sheet_line(sheet));
        shown += 1;
    }
    if shown == 0 {
        println!("{}", empty_list_message(&filters));
    }
    Ok(())
}

/// The "nothing matched" line, naming the active filters so an operator knows
/// why the list is empty.
fn empty_list_message(filters: &ListFilters<'_>) -> String {
    let mut qualifiers: Vec<String> = Vec::new();
    if let Some(entity_id) = filters.for_entity {
        qualifiers.push(format!("match {entity_id}"));
    }
    if filters.expired {
        qualifiers.push("are expired".to_owned());
    }
    if filters.stale {
        qualifiers.push(format!("are stale (> {} days)", filters.days));
    }
    if qualifiers.is_empty() {
        "(no guidance sheets)".to_owned()
    } else {
        format!("(no guidance sheets {})", qualifiers.join(" and "))
    }
}

/// Mint the `now − days` staleness cutoff via the connection's own clock, in the
/// same fixed-width ISO-8601 shape as [`now_iso8601`] so the storage staleness
/// predicate's lexical compare is a valid instant compare. `days` is a `u32`, so
/// the inline modifier string can never carry injection.
fn now_minus_days(conn: &Connection, days: u32) -> Result<String> {
    let ts: String = conn
        .query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now',?1)",
            [format!("-{days} days")],
            |row| row.get(0),
        )
        .context("mint staleness cutoff timestamp")?;
    Ok(ts)
}

fn delete(project_root: &Path, id: &str) -> Result<()> {
    let conn = open_db(project_root)?;

    // Snapshot the sheet (and thus its match_rules) BEFORE deletion so we can
    // still compute which entities it covered. Not-found is a clean error.
    let sheet = get_guidance_sheet(&conn, id)
        .into_anyhow()?
        .ok_or_else(|| anyhow!("guidance sheet {id} not found"))?;

    // ADR-007 churn-eager invalidation: removing the sheet removes guidance from
    // the entities it covered, so their cached summaries are stale and must be
    // dropped (the next query re-summarizes without the now-deleted guidance).
    //
    // ORDER MATTERS: invalidate BEFORE deleting the sheet row. The sheet's
    // `guides` edges are `from_id REFERENCES entities(id) ON DELETE CASCADE`, and
    // `open_db` enables `foreign_keys`, so deleting the sheet first would CASCADE
    // those edges away before `invalidate_summaries_for_sheet` reads them — and a
    // guides-only sheet would invalidate nothing. Invalidating first is safe:
    // rule/edge matching is unaffected by the sheet's presence (it never touches
    // the matched entities' own rows), and over-invalidation is harmless.
    let invalidated = invalidate_summaries_for_sheet(&conn, &sheet, project_root).into_anyhow()?;

    if !delete_guidance_sheet(&conn, id).into_anyhow()? {
        bail!("guidance sheet {id} not found")
    }

    println!("Deleted guidance sheet {id}");
    report_invalidation(invalidated);
    Ok(())
}

fn promote(project_root: &Path, config_path: Option<&Path>, observation_id: &str) -> Result<()> {
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mcp_config = crate::analyze::load_mcp_config(&canonical_root, config_path);
    let client = FiligreeHttpClient::from_config_with_project_root(
        &mcp_config.integrations.filigree,
        |name| std::env::var(name).ok(),
        Some(&canonical_root),
    )
    .context("build Filigree client")?
    .ok_or_else(|| anyhow!("Filigree integration is disabled in loomweave.yaml"))?;

    let observation = client
        .observation_by_id(observation_id)
        .with_context(|| format!("read Filigree observation {observation_id}"))?
        .ok_or_else(|| anyhow!("Filigree observation {observation_id} not found"))?;
    let proposal = GuidanceProposal::from_observation_detail(&observation.detail)
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| {
            format!("Filigree observation {observation_id} is not a Loomweave guidance proposal")
        })?;

    let conn = open_db(&canonical_root)?;
    let now = now_iso8601(&conn)?;
    let promoted = proposal
        .to_promoted_sheet(&now)
        .map_err(|e| anyhow!("{e}"))
        .context("build promoted guidance sheet")?;

    let before = get_guidance_sheet(&conn, &promoted.id).into_anyhow()?;
    upsert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: &promoted.id,
            name: &promoted.name,
            short_name: &promoted.short_name,
            properties: &promoted.properties,
        },
    )
    .into_anyhow()
    .with_context(|| format!("write promoted guidance sheet {}", promoted.id))?;

    let invalidated = match before {
        Some(before) => {
            invalidate_matched_summaries_union(&canonical_root, &conn, &before, &promoted.id)?
        }
        None => invalidate_matched_summaries(&canonical_root, &conn, &promoted.id)?,
    };

    let dismissed = client
        .dismiss_observation(observation_id, "promoted to Loomweave guidance sheet")
        .is_ok();
    println!("Promoted observation {observation_id} to {}", promoted.id);
    report_invalidation(invalidated);
    if !dismissed {
        println!(
            "Filigree observation {observation_id} was promoted locally but could not be dismissed"
        );
    }
    Ok(())
}

// ── Export / import (TDD target: round-trip) ──────────────────────────────────

/// Export every guidance sheet to `to_dir`, one deterministic JSON file per
/// sheet. The output is engineered to be committed to a shared git repo:
/// byte-identical across runs (sorted keys, no embedded export timestamp/path)
/// and diff-friendly (one file per sheet, one field per line). Sheets are
/// iterated in stable id order so any incidental logging is run-stable; the
/// per-file bytes — the thing that gets committed — carry no ordering.
fn export(project_root: &Path, to_dir: &Path) -> Result<()> {
    let conn = open_db(project_root)?;
    let mut sheets = list_guidance_sheets(&conn).into_anyhow()?;
    // `list_guidance_sheets` orders by scope_rank/authored_at/id (the read-path
    // composition sort). Re-sort by id alone for a stable, content-independent
    // export order — id is unique, so this is a total order with no tie-break on
    // a mutable field.
    sheets.sort_by(|a, b| a.id.cmp(&b.id));

    std::fs::create_dir_all(to_dir)
        .with_context(|| format!("create export directory {}", to_dir.display()))?;

    for sheet in &sheets {
        let portable = PortableSheet::from_sheet(sheet);
        let json = portable.to_canonical_json().into_anyhow()?;
        let file = to_dir.join(portable.file_name());
        std::fs::write(&file, json.as_bytes())
            .with_context(|| format!("write {}", file.display()))?;
    }

    println!(
        "Exported {} guidance sheet(s) to {}",
        sheets.len(),
        to_dir.display()
    );
    Ok(())
}

/// Import guidance sheets from `from_dir`. Reads every `*.json` file, parses each
/// into a [`PortableSheet`], and upserts it (additive — existing local sheets not
/// in the directory are untouched; this is a merge, never a destructive mirror).
/// Ids are preserved exactly. A malformed `*.json` aborts the whole import with
/// an error naming the file — a silently-dropped sheet is data loss, so we fail
/// loud rather than skip. Non-`.json` files (a README, a `.gitignore`) are
/// ignored: the sheet contract is `*.json`, so filtering to it is not "skipping a
/// sheet". Re-importing the same directory is idempotent on content.
fn import(project_root: &Path, from_dir: &Path) -> Result<()> {
    let conn = open_db(project_root)?;

    // Collect + sort the file list so import order (and thus any per-sheet log
    // line / cache-invalidation sequencing) is deterministic across runs.
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let entries = std::fs::read_dir(from_dir)
        .with_context(|| format!("read import directory {}", from_dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read entry in {}", from_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files.sort();

    let mut imported = 0usize;
    let mut invalidated = 0usize;
    for file in &files {
        let bytes =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let portable = PortableSheet::from_canonical_json(&file.display().to_string(), &bytes)
            .into_anyhow()?;

        // Snapshot the pre-import sheet (if any) BEFORE the upsert, so that when an
        // import UPDATES an existing sheet whose `match_rules` changed, we can
        // invalidate the OLD matches too — not just the post-import matches. A
        // fresh import (no prior sheet) has no old set.
        let before = get_guidance_sheet(&conn, &portable.id).into_anyhow()?;

        import_portable_sheet(&conn, &portable)
            .into_anyhow()
            .with_context(|| format!("import {}", file.display()))?;
        // ADR-007 churn-eager invalidation: an imported sheet adds/changes
        // guidance for the entities it covers, so their cached summaries must be
        // dropped — same union-of-before+after posture as `edit`.
        invalidated += match before {
            Some(before) => {
                invalidate_matched_summaries_union(project_root, &conn, &before, &portable.id)?
            }
            None => invalidate_matched_summaries(project_root, &conn, &portable.id)?,
        };
        imported += 1;
    }

    println!(
        "Imported {imported} guidance sheet(s) from {}",
        from_dir.display()
    );
    report_invalidation(invalidated);
    Ok(())
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

/// Open a read-write connection to `.loomweave/loomweave.db` with a generous busy
/// timeout so a concurrently-running `serve` writer does not cause an immediate
/// lock error.
fn open_db(project_root: &Path) -> Result<Connection> {
    let db_path = project_root.join(".loomweave").join("loomweave.db");
    if !db_path.exists() {
        bail!(
            "Loomweave database not found at {}; run `loomweave analyze` first",
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
    let file = dir.join(format!("loomweave-guidance-{}.md", std::process::id()));
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

/// Normalise an `--expires` value to a UTC instant in the exact
/// `YYYY-MM-DDTHH:MM:SS.mmmZ` shape the read path compares against. The expiry
/// check (`crates/loomweave-mcp/src/catalogue/inspection.rs`) is a *lexical*
/// `expires < now` compare, so the stored string must be byte-format-identical
/// to `now`: same UTC zone (`Z`), same 3-digit subsecond, same length. We run
/// the input through the connection's own `strftime`, which:
///   - accepts a full instant (`2026-12-31T23:59:59.999Z`), a date+time, an
///     offset form (`…+02:00`, converted to UTC), or a bare date;
///   - normalises a **date-only** value to **start-of-day UTC**
///     (`2026-06-03` → `2026-06-03T00:00:00.000Z`); and
///   - returns `NULL` for anything it cannot parse, which we reject.
///
/// # Errors
///
/// Returns an error if `raw` is not a parseable date/time.
fn normalize_expires(conn: &Connection, raw: &str) -> Result<String> {
    let normalized: Option<String> = conn
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1)", [raw], |row| {
            row.get(0)
        })
        .context("normalize --expires timestamp")?;
    normalized.ok_or_else(|| {
        anyhow!(
            "--expires '{raw}' is not a valid date/time; use an ISO-8601 instant \
             (e.g. 2026-12-31T23:59:59Z) or a date (e.g. 2026-12-31, taken as \
             start-of-day UTC)"
        )
    })
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

    #[test]
    fn normalize_expires_produces_now_compatible_format() {
        let conn = Connection::open_in_memory().unwrap();
        // A full instant round-trips byte-identically.
        assert_eq!(
            normalize_expires(&conn, "2026-12-31T23:59:59.999Z").unwrap(),
            "2026-12-31T23:59:59.999Z"
        );
        // A bare date normalizes to start-of-day UTC, NOT a bare prefix that
        // would sort below same-day instants and expire immediately.
        assert_eq!(
            normalize_expires(&conn, "2026-12-31").unwrap(),
            "2026-12-31T00:00:00.000Z"
        );
        // An offset form is converted to UTC `Z`.
        assert_eq!(
            normalize_expires(&conn, "2026-06-03T12:00:00+02:00").unwrap(),
            "2026-06-03T10:00:00.000Z"
        );
        // Every normalized value matches the `now` shape (24 chars, ends in Z).
        for raw in ["2026-12-31", "2026-12-31T23:59:59Z", "2026-06-03 12:00:00"] {
            let out = normalize_expires(&conn, raw).unwrap();
            assert_eq!(out.len(), 24, "{raw} -> {out}");
            assert!(out.ends_with('Z'), "{raw} -> {out}");
        }
    }

    #[test]
    fn normalize_expires_rejects_garbage() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(normalize_expires(&conn, "tomorrow").is_err());
        assert!(normalize_expires(&conn, "not-a-date").is_err());
        assert!(normalize_expires(&conn, "").is_err());
    }

    #[test]
    fn normalize_expires_future_is_not_lexically_expired() {
        // Proxy the read path's `expires < now` lexical compare: a future
        // normalized expiry must sort *after* the current instant, so the read
        // path will NOT treat the sheet as expired.
        let conn = Connection::open_in_memory().unwrap();
        let now = now_iso8601(&conn).unwrap();
        let future = normalize_expires(&conn, "2999-01-01T00:00:00Z").unwrap();
        assert!(
            future > now,
            "future expiry {future} must sort after now {now}"
        );
    }
}
