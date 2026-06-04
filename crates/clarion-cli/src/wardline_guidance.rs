use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use clarion_storage::{
    GuidanceSheetInput, get_guidance_sheet, invalidate_summaries_for_sheet, slugify_guidance_name,
    upsert_guidance_sheet,
};

const PROVENANCE_DERIVED: &str = "wardline_derived";
const PROVENANCE_OVERRIDDEN: &str = "wardline_derived_overridden";

#[derive(Default)]
pub(crate) struct WardlineGuidanceStats {
    pub(crate) generated: usize,
    pub(crate) overridden: usize,
}

#[derive(Debug, Default)]
struct WardlineManifest {
    tier_entries: BTreeMap<String, WardlineGuidanceEntry>,
    tier_definitions: BTreeMap<String, WardlineTierDefinition>,
    module_tiers: Vec<WardlineModuleTier>,
    boundaries: BTreeMap<String, WardlineGuidanceEntry>,
    annotation_groups: BTreeMap<String, WardlineGuidanceEntry>,
    fingerprint: Option<WardlineFingerprint>,
    exceptions: Option<WardlineExceptions>,
    overlay_boundaries: Vec<WardlineOverlayBoundary>,
    artifact_hashes: WardlineArtifactHashes,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawWardlineManifest {
    tiers: Option<Value>,
    module_tiers: Vec<WardlineModuleTier>,
    #[serde(alias = "boundary_contracts")]
    boundaries: BTreeMap<String, WardlineGuidanceEntry>,
    #[serde(alias = "groups")]
    annotation_groups: BTreeMap<String, WardlineGuidanceEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WardlineGuidanceEntry {
    paths: Vec<String>,
    content: Option<String>,
    scope_level: Option<String>,
    match_rules: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct WardlineTierDefinition {
    id: String,
    tier: Option<u8>,
    description: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct WardlineModuleTier {
    path: String,
    default_taint: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct WardlineFingerprint {
    fingerprints: Vec<WardlineFingerprintEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct WardlineFingerprintEntry {
    decorators: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct WardlineExceptions {
    exceptions: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct WardlineOverlay {
    overlay_for: Option<String>,
    boundaries: Vec<WardlineOverlayBoundaryEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct WardlineOverlayBoundaryEntry {
    function: String,
    transition: String,
    from_tier: Option<u8>,
    to_tier: Option<u8>,
    restored_tier: Option<u8>,
    bounded_context: Option<Value>,
}

#[derive(Debug, Clone)]
struct WardlineOverlayBoundary {
    scope: String,
    entry: WardlineOverlayBoundaryEntry,
}

#[derive(Debug, Clone, Default)]
struct WardlineArtifactHashes {
    root_manifest_hash: String,
    fingerprint_hash: Option<String>,
    exceptions_hash: Option<String>,
    overlay_hashes: Vec<(String, String)>,
}

struct GeneratedSheet {
    id: String,
    name: String,
    short_name: String,
    properties: Value,
}

pub(crate) fn sync_wardline_guidance(
    db_path: &Path,
    project_root: &Path,
) -> Result<WardlineGuidanceStats> {
    let Some((manifest_hash, manifest)) = read_manifest(project_root)? else {
        return Ok(WardlineGuidanceStats::default());
    };
    let generated = generated_sheets(&manifest, &manifest_hash);
    if generated.is_empty() {
        return Ok(WardlineGuidanceStats::default());
    }

    let conn = open_write_connection(db_path)?;
    let now = now_iso8601(&conn)?;
    let mut stats = WardlineGuidanceStats::default();
    for mut sheet in generated {
        if let Some(obj) = sheet.properties.as_object_mut() {
            obj.insert("authored_at".to_owned(), json!(now));
        }

        let before =
            get_guidance_sheet(&conn, &sheet.id).map_err(|err| anyhow::anyhow!("{err}"))?;
        let mut write_sheet = true;
        if let Some(existing) = before.as_ref() {
            let stored_signature = existing
                .properties
                .get("wardline_generated_signature")
                .and_then(Value::as_str);
            let actual_signature = derived_signature_from_properties(&existing.properties);
            let edited = stored_signature != Some(actual_signature.as_str());
            if edited {
                let mut properties = existing.properties.clone();
                if let Some(obj) = properties.as_object_mut() {
                    obj.insert("provenance".to_owned(), json!(PROVENANCE_OVERRIDDEN));
                }
                sheet.properties = properties;
                write_sheet = true;
                stats.overridden += 1;
            }
        }

        if write_sheet {
            upsert_guidance_sheet(
                &conn,
                &GuidanceSheetInput {
                    id: &sheet.id,
                    name: &sheet.name,
                    short_name: &sheet.short_name,
                    properties: &sheet.properties,
                },
            )
            .map_err(|err| anyhow::anyhow!("{err}"))
            .with_context(|| format!("write Wardline-derived guidance {}", sheet.id))?;

            if let Some(before) = before.as_ref() {
                let _ = invalidate_summaries_for_sheet(&conn, before, project_root);
            }
            if let Some(after) =
                get_guidance_sheet(&conn, &sheet.id).map_err(|err| anyhow::anyhow!("{err}"))?
            {
                let _ = invalidate_summaries_for_sheet(&conn, &after, project_root);
            }
            stats.generated += 1;
        }
    }
    Ok(stats)
}

pub(crate) fn current_manifest_hash(project_root: &Path) -> Result<Option<String>> {
    Ok(read_manifest(project_root)?.map(|(hash, _)| hash))
}

fn read_manifest(project_root: &Path) -> Result<Option<(String, WardlineManifest)>> {
    let path = project_root.join("wardline.yaml");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read Wardline manifest {}", path.display()))?;
    let raw_manifest: RawWardlineManifest = serde_norway::from_str(&raw)
        .with_context(|| format!("parse Wardline manifest {}", path.display()))?;
    let mut manifest = WardlineManifest::from_raw(raw_manifest)?;
    let root_manifest_hash = hash_bytes(raw.as_bytes());
    let mut hash_parts = vec![("wardline.yaml".to_owned(), raw.into_bytes())];

    let fingerprint = read_optional_json::<WardlineFingerprint>(
        project_root,
        &["wardline.fingerprint.json", "fingerprint.json"],
    )?;
    if let Some(artifact) = fingerprint {
        hash_parts.push((artifact.relative_path.clone(), artifact.raw.into_bytes()));
        manifest.artifact_hashes.fingerprint_hash = Some(artifact.hash);
        manifest.fingerprint = Some(artifact.parsed);
    }

    let exceptions =
        read_optional_json::<WardlineExceptions>(project_root, &["wardline.exceptions.json"])?;
    if let Some(artifact) = exceptions {
        hash_parts.push((artifact.relative_path.clone(), artifact.raw.into_bytes()));
        manifest.artifact_hashes.exceptions_hash = Some(artifact.hash);
        manifest.exceptions = Some(artifact.parsed);
    }

    for artifact in read_overlay_artifacts(project_root)? {
        hash_parts.push((artifact.relative_path.clone(), artifact.raw.into_bytes()));
        manifest
            .artifact_hashes
            .overlay_hashes
            .push((artifact.relative_path, artifact.hash));
        let scope = artifact.parsed.overlay_for.unwrap_or_default();
        manifest.overlay_boundaries.extend(
            artifact
                .parsed
                .boundaries
                .into_iter()
                .filter(|entry| !entry.function.trim().is_empty())
                .map(|entry| WardlineOverlayBoundary {
                    scope: scope.clone(),
                    entry,
                }),
        );
    }

    manifest.artifact_hashes.root_manifest_hash = root_manifest_hash;
    let hash = bundle_hash(&hash_parts);
    Ok(Some((hash, manifest)))
}

impl WardlineManifest {
    fn from_raw(raw: RawWardlineManifest) -> Result<Self> {
        let (tier_entries, tier_definitions) = parse_tiers(raw.tiers)?;
        Ok(Self {
            tier_entries,
            tier_definitions,
            module_tiers: raw.module_tiers,
            boundaries: raw.boundaries,
            annotation_groups: raw.annotation_groups,
            ..Self::default()
        })
    }
}

fn parse_tiers(
    tiers: Option<Value>,
) -> Result<(
    BTreeMap<String, WardlineGuidanceEntry>,
    BTreeMap<String, WardlineTierDefinition>,
)> {
    let Some(tiers) = tiers else {
        return Ok((BTreeMap::new(), BTreeMap::new()));
    };
    if tiers.is_object() {
        let entries = serde_json::from_value::<BTreeMap<String, WardlineGuidanceEntry>>(tiers)
            .context("parse Wardline guidance-style tier map")?;
        return Ok((entries, BTreeMap::new()));
    }
    if tiers.is_array() {
        let definitions = serde_json::from_value::<Vec<WardlineTierDefinition>>(tiers)
            .context("parse Wardline tier definitions")?
            .into_iter()
            .filter(|definition| !definition.id.trim().is_empty())
            .map(|definition| (definition.id.clone(), definition))
            .collect();
        return Ok((BTreeMap::new(), definitions));
    }
    anyhow::bail!("Wardline tiers must be either a guidance map or a tier-definition array");
}

struct ParsedArtifact<T> {
    relative_path: String,
    raw: String,
    hash: String,
    parsed: T,
}

fn read_optional_json<T>(
    project_root: &Path,
    candidates: &[&str],
) -> Result<Option<ParsedArtifact<T>>>
where
    T: for<'de> Deserialize<'de>,
{
    for candidate in candidates {
        let path = project_root.join(candidate);
        if !path.exists() {
            continue;
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        return Ok(Some(ParsedArtifact {
            relative_path: (*candidate).to_owned(),
            hash: hash_bytes(raw.as_bytes()),
            raw,
            parsed,
        }));
    }
    Ok(None)
}

fn read_overlay_artifacts(project_root: &Path) -> Result<Vec<ParsedArtifact<WardlineOverlay>>> {
    let mut paths = Vec::new();
    collect_overlay_paths(project_root, &mut paths)?;
    paths.sort();
    let mut overlays = Vec::new();
    for path in paths {
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let parsed: WardlineOverlay =
            serde_norway::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        let relative_path = path
            .strip_prefix(project_root)
            .ok()
            .and_then(|rel| rel.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("wardline.overlay.yaml"))
            .replace('\\', "/");
        let scope = overlay_scope(&relative_path, parsed.overlay_for.as_deref());
        overlays.push(ParsedArtifact {
            relative_path,
            hash: hash_bytes(raw.as_bytes()),
            raw,
            parsed: WardlineOverlay {
                overlay_for: Some(scope),
                boundaries: parsed.boundaries,
            },
        });
    }
    Ok(overlays)
}

fn collect_overlay_paths(dir: &Path, paths: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("read directory {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read directory entry {}", dir.display()))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if matches!(
                file_name.as_ref(),
                ".git" | ".clarion" | ".venv" | "target" | "node_modules"
            ) {
                continue;
            }
            collect_overlay_paths(&path, paths)?;
        } else if file_name == "wardline.overlay.yaml" {
            paths.push(path);
        }
    }
    Ok(())
}

fn overlay_scope(relative_path: &str, overlay_for: Option<&str>) -> String {
    if let Some(scope) = overlay_for.map(str::trim)
        && !scope.is_empty()
        && scope != "."
        && scope != "wardline.yaml"
    {
        return scope.trim_matches('/').to_owned();
    }
    Path::new(relative_path)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or("")
        .trim_matches('/')
        .replace('\\', "/")
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn bundle_hash(parts: &[(String, Vec<u8>)]) -> String {
    let mut hasher = blake3::Hasher::new();
    for (label, bytes) in parts {
        hasher.update(label.as_bytes());
        hasher.update(&[0]);
        hasher.update(bytes);
        hasher.update(&[0xff]);
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn generated_sheets(manifest: &WardlineManifest, manifest_hash: &str) -> Vec<GeneratedSheet> {
    let mut sheets = Vec::new();
    let artifact_properties = artifact_properties(manifest);
    if manifest.module_tiers.is_empty() {
        for (key, entry) in &manifest.tier_entries {
            sheets.push(generated_sheet(
                "tier",
                key,
                entry,
                manifest_hash,
                &artifact_properties,
                &format!(
                    "Wardline tier `{key}` applies here. Preserve its trust-boundary intent when summarising or changing this code."
                ),
            ));
        }
    } else {
        for assignment in &manifest.module_tiers {
            if assignment.path.trim().is_empty() || assignment.default_taint.trim().is_empty() {
                continue;
            }
            let key = format!("{}-{}", assignment.path, assignment.default_taint);
            let tier = manifest.tier_definitions.get(&assignment.default_taint);
            let tier_number = tier.and_then(|definition| definition.tier);
            let description = tier.and_then(|definition| definition.description.as_deref());
            let content = module_tier_content(assignment, tier_number, description);
            let entry = WardlineGuidanceEntry {
                paths: vec![path_glob(&assignment.path)],
                content: Some(content),
                scope_level: Some("module".to_owned()),
                match_rules: None,
            };
            sheets.push(generated_sheet(
                "tier",
                &key,
                &entry,
                manifest_hash,
                &artifact_properties,
                "",
            ));
        }
    }
    for (key, entry) in &manifest.boundaries {
        sheets.push(generated_sheet(
            "boundary",
            key,
            entry,
            manifest_hash,
            &artifact_properties,
            &format!(
                "Wardline boundary contract `{key}` applies here. Call out boundary assumptions and cross-boundary effects."
            ),
        ));
    }
    for boundary in &manifest.overlay_boundaries {
        sheets.push(generated_overlay_boundary_sheet(
            boundary,
            manifest_hash,
            &artifact_properties,
        ));
    }
    for (key, entry) in &manifest.annotation_groups {
        sheets.push(generated_sheet(
            "annotation_group",
            key,
            entry,
            manifest_hash,
            &artifact_properties,
            &format!(
                "Wardline annotation group `{key}` applies here. Preserve the group-specific review context."
            ),
        ));
    }
    for decorator in fingerprint_decorators(manifest) {
        let entry = WardlineGuidanceEntry {
            paths: Vec::new(),
            content: Some(format!(
                "Wardline annotation group `{decorator}` is present in the fingerprint baseline. Preserve its Wardline review semantics when interpreting affected code."
            )),
            scope_level: Some("project".to_owned()),
            match_rules: Some(vec![json!({"type": "wardline_group", "name": decorator})]),
        };
        sheets.push(generated_sheet(
            "annotation_group",
            &decorator,
            &entry,
            manifest_hash,
            &artifact_properties,
            "",
        ));
    }
    sheets.sort_by(|a, b| a.id.cmp(&b.id));
    sheets
}

fn artifact_properties(manifest: &WardlineManifest) -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert(
        "wardline_root_manifest_hash".to_owned(),
        json!(manifest.artifact_hashes.root_manifest_hash),
    );
    if let Some(hash) = &manifest.artifact_hashes.fingerprint_hash {
        properties.insert("wardline_fingerprint_hash".to_owned(), json!(hash));
        properties.insert(
            "wardline_fingerprint_count".to_owned(),
            json!(
                manifest
                    .fingerprint
                    .as_ref()
                    .map_or(0, |fingerprint| fingerprint.fingerprints.len())
            ),
        );
    }
    if let Some(hash) = &manifest.artifact_hashes.exceptions_hash {
        properties.insert("wardline_exceptions_hash".to_owned(), json!(hash));
        properties.insert(
            "wardline_exception_count".to_owned(),
            json!(
                manifest
                    .exceptions
                    .as_ref()
                    .map_or(0, |exceptions| exceptions.exceptions.len())
            ),
        );
    }
    if !manifest.artifact_hashes.overlay_hashes.is_empty() {
        let overlays: Vec<Value> = manifest
            .artifact_hashes
            .overlay_hashes
            .iter()
            .map(|(path, hash)| json!({"path": path, "hash": hash}))
            .collect();
        properties.insert("wardline_overlay_hashes".to_owned(), json!(overlays));
    }
    properties
}

fn fingerprint_decorators(manifest: &WardlineManifest) -> BTreeSet<String> {
    manifest
        .fingerprint
        .as_ref()
        .into_iter()
        .flat_map(|fingerprint| &fingerprint.fingerprints)
        .flat_map(|entry| &entry.decorators)
        .filter(|decorator| !decorator.trim().is_empty())
        .cloned()
        .collect()
}

fn module_tier_content(
    assignment: &WardlineModuleTier,
    tier_number: Option<u8>,
    description: Option<&str>,
) -> String {
    let tier = tier_number.map_or_else(
        || assignment.default_taint.clone(),
        |number| format!("Tier {number} ({})", assignment.default_taint),
    );
    let description = description
        .filter(|description| !description.trim().is_empty())
        .map(|description| format!(" {description}"))
        .unwrap_or_default();
    format!(
        "Wardline assigns `{}` to `{}` as {tier}.{description} Preserve its trust-boundary intent when summarising or changing this code.",
        assignment.default_taint, assignment.path
    )
}

fn path_glob(path: &str) -> String {
    let path = path.trim().trim_matches('/');
    if path.is_empty() {
        "**".to_owned()
    } else if path.contains('*') || path.contains('?') {
        path.to_owned()
    } else {
        format!("{path}/**")
    }
}

fn generated_overlay_boundary_sheet(
    boundary: &WardlineOverlayBoundary,
    manifest_hash: &str,
    artifact_properties: &Map<String, Value>,
) -> GeneratedSheet {
    let key = format!("{}-{}", boundary.scope, boundary.entry.function);
    let mut details = Vec::new();
    if let Some(from_tier) = boundary.entry.from_tier {
        details.push(format!("from Tier {from_tier}"));
    }
    if let Some(to_tier) = boundary.entry.to_tier {
        details.push(format!("to Tier {to_tier}"));
    }
    if let Some(restored_tier) = boundary.entry.restored_tier {
        details.push(format!("restoring Tier {restored_tier}"));
    }
    if boundary.entry.bounded_context.is_some() {
        details.push("with bounded-context contracts".to_owned());
    }
    let suffix = if details.is_empty() {
        String::new()
    } else {
        format!(" ({})", details.join(", "))
    };
    let entry = WardlineGuidanceEntry {
        paths: vec![path_glob(&boundary.scope)],
        content: Some(format!(
            "Wardline boundary `{}` in `{}` declares transition `{}`{suffix}. Call out boundary assumptions and cross-boundary effects.",
            boundary.entry.function, boundary.scope, boundary.entry.transition
        )),
        scope_level: Some("subsystem".to_owned()),
        match_rules: None,
    };
    generated_sheet(
        "boundary",
        &key,
        &entry,
        manifest_hash,
        artifact_properties,
        "",
    )
}

fn generated_sheet(
    kind: &str,
    key: &str,
    entry: &WardlineGuidanceEntry,
    manifest_hash: &str,
    artifact_properties: &Map<String, Value>,
    default_content: &str,
) -> GeneratedSheet {
    let name = slugify_guidance_name(&format!("wardline-{}-{key}", kind.replace('_', "-")));
    let id = format!("core:guidance:{name}");
    let short_name = name.rsplit('.').next().unwrap_or(&name).to_owned();
    let match_rules = entry.match_rules.clone().unwrap_or_else(|| {
        entry
            .paths
            .iter()
            .map(|path| json!({ "type": "path", "pattern": path }))
            .collect()
    });
    let content = entry
        .content
        .clone()
        .unwrap_or_else(|| default_content.to_owned());
    let scope_level = entry
        .scope_level
        .clone()
        .unwrap_or_else(|| "module".to_owned());
    let signature = derived_signature(&content, &scope_level, &match_rules, kind, key);
    let mut properties = json!({
        "content": content,
        "scope_level": scope_level,
        "match_rules": match_rules,
        "pinned": true,
        "provenance": PROVENANCE_DERIVED,
        "wardline_kind": kind,
        "wardline_key": key,
        "wardline_manifest_hash": manifest_hash,
        "wardline_generated_signature": signature,
    });
    if let Some(obj) = properties.as_object_mut() {
        obj.extend(artifact_properties.clone());
    }
    GeneratedSheet {
        id,
        name,
        short_name,
        properties,
    }
}

pub(crate) fn is_wardline_derived(properties: &Value) -> bool {
    matches!(
        properties.get("provenance").and_then(Value::as_str),
        Some(PROVENANCE_DERIVED | PROVENANCE_OVERRIDDEN)
    )
}

fn derived_signature_from_properties(properties: &Value) -> String {
    let content = properties
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let scope_level = properties
        .get("scope_level")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let match_rules = properties
        .get("match_rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let kind = properties
        .get("wardline_kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let key = properties
        .get("wardline_key")
        .and_then(Value::as_str)
        .unwrap_or_default();
    derived_signature(content, scope_level, &match_rules, kind, key)
}

fn derived_signature(
    content: &str,
    scope_level: &str,
    match_rules: &[Value],
    kind: &str,
    key: &str,
) -> String {
    let payload = json!({
        "content": content,
        "scope_level": scope_level,
        "match_rules": match_rules,
        "pinned": true,
        "wardline_kind": kind,
        "wardline_key": key,
    });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    format!("blake3:{}", blake3::hash(&bytes).to_hex())
}

fn open_write_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open database {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("set busy_timeout")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("enable foreign_keys")?;
    Ok(conn)
}

fn now_iso8601(conn: &Connection) -> Result<String> {
    let ts: String = conn
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
            row.get(0)
        })
        .context("mint guidance timestamp")?;
    Ok(ts)
}

#[cfg(test)]
mod tests {
    use super::{overlay_scope, path_glob};
    use clarion_storage::glob_match;

    #[test]
    fn root_overlay_scope_glob_matches_project_relative_paths() {
        let scope = overlay_scope("wardline.overlay.yaml", None);
        let pattern = path_glob(&scope);

        assert_eq!(scope, "");
        assert_eq!(pattern, "**");
        assert!(glob_match(&pattern, "src/foo.py"));
        assert!(glob_match(&pattern, "foo.py"));
    }
}
