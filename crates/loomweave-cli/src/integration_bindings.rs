//! Local three-way Loomweave/Filigree/Wardline dogfood bindings.
//!
//! These are intentionally configuration bindings, not a shared runtime:
//! Loomweave enables its own optional HTTP and Filigree read surfaces, and the
//! project-local `.mcp.json` launches Wardline with the two peer URLs as
//! `--loomweave-url` / `--filigree-url` flags for MCP scans.
//!
//! Wardline receives those URLs *only* via the `.mcp.json` launch flags (its
//! `resolve_loomweave_url` / `resolve_filigree_url` precedence is
//! flag > env > published `.weft/*/ephemeral.port`). It reads no URL from any
//! `wardline.yaml` — that file is not in either resolver's chain — so Loomweave
//! does not write one. (The separate, now-orphaned `wardline.yaml` *manifest*
//! read on the analyze side is tracked in clarion-7c9336163e.)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

const DEFAULT_FILIGREE_BASE_URL: &str = "http://127.0.0.1:8766";

/// ADR-044 migration: older `install --all` runs unconditionally stamped a fixed
/// `serve.http.bind: 127.0.0.1:9111`. The deterministic read-API band is
/// `9400–10399`, so this exact literal can only be the old auto-default, never a
/// deterministic value. We strip it on repair so auto-port + ephemeral fallback
/// can engage; any other (operator-chosen) bind is left intact.
const STALE_DEFAULT_BIND: &str = "127.0.0.1:9111";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
    Present,
    MissingOrStale,
    Unparseable,
}

// All three fields are URLs by nature; the `_url` suffix is the meaningful part
// of each name, not redundant noise.
#[allow(clippy::struct_field_names)]
struct DesiredBindings {
    filigree_base_url: String,
    wardline_filigree_url: String,
    loomweave_url: String,
}

/// Classify the local three-way integration binding files without writing.
#[must_use]
pub fn binding_state(project_root: &Path) -> BindingState {
    let desired = desired_bindings(project_root);
    match (
        loomweave_yaml_ok(project_root, &desired),
        wardline_mcp_ok(project_root, &desired),
    ) {
        (Ok(true), Ok(true)) => BindingState::Present,
        (Err(_), _) | (_, Err(_)) => BindingState::Unparseable,
        _ => BindingState::MissingOrStale,
    }
}

/// Repair the local three-way integration binding files. Returns true if any
/// file changed.
///
/// # Errors
///
/// Returns an error when an existing config file is malformed or has a shape we
/// cannot merge without clobbering unrelated user content.
pub fn install_bindings(project_root: &Path) -> Result<bool> {
    let desired = desired_bindings(project_root);
    let mut changed = false;
    changed |= install_loomweave_yaml(project_root, &desired)?;
    changed |= install_wardline_mcp(project_root, &desired)?;
    Ok(changed)
}

fn desired_bindings(project_root: &Path) -> DesiredBindings {
    let filigree_base_url = live_filigree_base_url(project_root)
        .or_else(|| configured_filigree_base_url(project_root))
        .unwrap_or_else(|| DEFAULT_FILIGREE_BASE_URL.to_owned());
    // Server-mode Filigree mounts the federation write router under
    // `/api/p/{prefix}/…` and fail-closes an unscoped write (filigree N1), so the
    // bridge URL must carry the project scope or every wardline scan 400s. A
    // single-project (non-server) Filigree, or no Filigree at all, keeps the
    // unscoped `/api/…` mount. (gap-analysis opp #2 / weft path-scope action.)
    let base = filigree_base_url.trim_end_matches('/');
    let wardline_filigree_url = match filigree_server_scope(project_root) {
        Some(prefix) => format!("{base}/api/p/{prefix}/weft/scan-results"),
        None => format!("{base}/api/weft/scan-results"),
    };
    // ADR-044: seed the consumer's static target with this project's
    // deterministic read-API port. serve binds the same port (barring an
    // ephemeral fallback), and the published .weft/loomweave/ephemeral.port file
    // overrides this at runtime once a consumer resolves consume-time.
    let port = loomweave_federation::loomweave_port::deterministic_port(project_root);
    let loomweave_url = format!("http://127.0.0.1:{port}");
    DesiredBindings {
        filigree_base_url,
        wardline_filigree_url,
        loomweave_url,
    }
}

/// This project's Filigree routing key, but only when Filigree runs in *server*
/// mode (the case that requires a project-scoped `/api/p/{prefix}/…` write).
///
/// Reads `.weft/filigree/config.json`; the URL-facing key is `prefix` (filigree
/// routes `/api/p/{prefix}` on it; `name` is display-only, kept as a fallback).
/// Fail-soft: returns `None` (→ unscoped path) when the config is absent,
/// unparseable, not `mode: "server"`, or carries no usable key — so a
/// Loomweave-solo or single-project layout is unchanged.
fn filigree_server_scope(project_root: &Path) -> Option<String> {
    let path = project_root.join(".weft/filigree/config.json");
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    if value.get("mode").and_then(Value::as_str) != Some("server") {
        return None;
    }
    value
        .get("prefix")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
}

fn live_filigree_base_url(project_root: &Path) -> Option<String> {
    // ADR-046: read Filigree's live port only from the consolidated
    // `.weft/filigree/ephemeral.port` location, via the canonical resolver so the
    // single-location policy stays in one place. No `.filigree/` legacy fallback.
    let port = loomweave_federation::filigree_url::read_filigree_ephemeral_port(project_root)?;
    Some(format!("http://127.0.0.1:{port}"))
}

fn configured_filigree_base_url(project_root: &Path) -> Option<String> {
    let path = project_root.join("loomweave.yaml");
    let value = read_yaml_value(&path).ok()?;
    value
        .get("integrations")
        .and_then(|integrations| integrations.get("filigree"))
        .and_then(|filigree| filigree.get("base_url"))
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(str::to_owned)
}

fn loomweave_yaml_ok(project_root: &Path, desired: &DesiredBindings) -> Result<bool> {
    let path = project_root.join("loomweave.yaml");
    if !path.exists() {
        return Ok(false);
    }
    let value = read_yaml_value(&path)?;
    Ok(value
        .get("integrations")
        .and_then(|integrations| integrations.get("filigree"))
        .is_some_and(|filigree| {
            filigree.get("enabled").and_then(Value::as_bool) == Some(true)
                && filigree.get("base_url").and_then(Value::as_str)
                    == Some(desired.filigree_base_url.as_str())
                && filigree
                    .get("actor")
                    .and_then(Value::as_str)
                    .is_some_and(|actor| !actor.trim().is_empty())
        })
        && value
            .get("serve")
            .and_then(|serve| serve.get("http"))
            .is_some_and(|http| {
                http.get("enabled").and_then(Value::as_bool) == Some(true)
                    && http.get("wardline_taint_write").and_then(Value::as_bool) == Some(true)
                    && http.get("bind").and_then(Value::as_str) != Some(STALE_DEFAULT_BIND)
            }))
}

fn wardline_mcp_ok(project_root: &Path, desired: &DesiredBindings) -> Result<bool> {
    let path = project_root.join(".mcp.json");
    if !path.exists() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    if !value.is_object() {
        bail!("{} top-level JSON is not an object", path.display());
    }
    let Some(servers) = value.get("mcpServers") else {
        return Ok(false);
    };
    if !servers.is_object() {
        bail!("{} mcpServers is not an object", path.display());
    }
    let Some(entry) = servers.get("wardline") else {
        return Ok(false);
    };
    // Compare against the *effective* args: a scoped emit URL already in place is
    // not "stale" just because Loomweave would otherwise compute an unscoped one,
    // so the no-clobber guard must not make doctor flap (check ↔ write parity).
    let existing = existing_wardline_filigree_url(&value);
    let filigree_url = effective_wardline_filigree_url(desired, existing.as_deref());
    Ok(entry.get("args") == Some(&desired_wardline_args(desired, &filigree_url)))
}

fn install_loomweave_yaml(project_root: &Path, desired: &DesiredBindings) -> Result<bool> {
    let path = project_root.join("loomweave.yaml");
    let mut value = read_yaml_value_or_empty(&path)?;
    let root = object_mut(&mut value, &path)?;
    root.entry("version".to_owned()).or_insert(json!(1));
    let integrations = ensure_object(root, "integrations")?;
    let filigree = ensure_object(integrations, "filigree")?;
    filigree.insert("enabled".to_owned(), json!(true));
    filigree.insert("base_url".to_owned(), json!(desired.filigree_base_url));
    ensure_string(filigree, "actor", "loomweave-mcp");
    ensure_string(filigree, "token_env", "WEFT_FEDERATION_TOKEN");
    filigree
        .entry("timeout_seconds".to_owned())
        .or_insert(json!(5));

    let serve = ensure_object(root, "serve")?;
    let http = ensure_object(serve, "http")?;
    // ADR-044 migration: strip exactly the old auto-stamped `bind: 127.0.0.1:9111`
    // so auto-port + ephemeral fallback can engage. A deliberately operator-chosen
    // bind (any other value) is left intact.
    if http.get("bind").and_then(Value::as_str) == Some(STALE_DEFAULT_BIND) {
        http.remove("bind");
    }
    http.insert("enabled".to_owned(), json!(true));
    http.insert("wardline_taint_write".to_owned(), json!(true));
    write_yaml_if_changed(&path, &value)
}

fn install_wardline_mcp(project_root: &Path, desired: &DesiredBindings) -> Result<bool> {
    let path = project_root.join(".mcp.json");
    let mut root = if path.exists() {
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if raw.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?
        }
    } else {
        Value::Object(Map::new())
    };
    if !root.is_object() {
        bail!(
            "refusing to rewrite {}: top-level JSON is not an object",
            path.display()
        );
    }
    if let Some(servers) = root.get("mcpServers")
        && !servers.is_object()
    {
        bail!(
            "refusing to rewrite {}: `mcpServers` is present but is not an object",
            path.display()
        );
    }
    // Read the existing emit URL *before* mutating, so the no-clobber guard can
    // preserve a scoped value rather than downgrade it to the unscoped form.
    let existing = existing_wardline_filigree_url(&root);
    let filigree_url = effective_wardline_filigree_url(desired, existing.as_deref());
    let root_obj = root.as_object_mut().expect("root is object");
    let servers = root_obj
        .entry("mcpServers".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers.as_object_mut().expect("mcpServers is object");
    let desired_entry = json!({
        "type": "stdio",
        "command": wardline_command(),
        "args": desired_wardline_args(desired, &filigree_url),
    });
    if servers.get("wardline") == Some(&desired_entry) {
        return Ok(false);
    }
    servers.insert("wardline".to_owned(), desired_entry);
    write_json_if_changed(&path, &root)
}

fn desired_wardline_args(desired: &DesiredBindings, filigree_url: &str) -> Value {
    json!([
        "mcp",
        "--root",
        ".",
        "--loomweave-url",
        desired.loomweave_url,
        "--filigree-url",
        filigree_url
    ])
}

/// A scan-results URL is project-scoped when it carries the server-mode
/// `/api/p/<project>/` mount. The shared `--server-mode` daemon **fail-closes an
/// unscoped write with a 400 that looks like a successful POST**, silently
/// dropping every finding (weft emit incident, 2026-06-10).
fn is_scoped_scan_url(url: &str) -> bool {
    url.contains("/api/p/")
}

/// The `--filigree-url` value currently recorded in the project's `.mcp.json`
/// wardline entry, if any.
fn existing_wardline_filigree_url(root: &Value) -> Option<String> {
    let args = root
        .get("mcpServers")?
        .get("wardline")?
        .get("args")?
        .as_array()?;
    let flag = args
        .iter()
        .position(|arg| arg.as_str() == Some("--filigree-url"))?;
    args.get(flag + 1)?.as_str().map(str::to_owned)
}

/// No-clobber guard: Loomweave must never replace an existing **scoped** emit URL
/// with its own **unscoped** computation. When Loomweave cannot itself determine
/// the project scope — it has no server-mode marker to read yet (the per-store
/// server-mode marker is a pending Filigree producer-side contract) —
/// `desired.wardline_filigree_url` falls back to the unscoped form; writing that
/// over a good scoped value is exactly what makes
/// findings vanish. So if the desired value is unscoped and the project already
/// carries a scoped one, keep theirs. (Wardline's own installer owns the scoped
/// value; this guard just stops Loomweave from downgrading it.)
fn effective_wardline_filigree_url(desired: &DesiredBindings, existing: Option<&str>) -> String {
    match existing {
        Some(url)
            if !is_scoped_scan_url(&desired.wardline_filigree_url) && is_scoped_scan_url(url) =>
        {
            url.to_owned()
        }
        _ => desired.wardline_filigree_url.clone(),
    }
}

fn wardline_command() -> String {
    if let Some(home) = env::var_os("HOME") {
        for name in ["wardline", "wardline.exe"] {
            let candidate = PathBuf::from(&home).join(".local/bin").join(name);
            if candidate.is_file() {
                return candidate.display().to_string();
            }
        }
    }
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            for name in ["wardline", "wardline.exe"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate.display().to_string();
                }
            }
        }
    }
    "wardline".to_owned()
}

fn read_yaml_value(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_norway::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn read_yaml_value_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        Ok(Value::Object(Map::new()))
    } else {
        serde_norway::from_str(&raw).with_context(|| format!("parse {}", path.display()))
    }
}

fn object_mut<'a>(value: &'a mut Value, path: &Path) -> Result<&'a mut Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} top-level YAML is not a mapping", path.display()))
}

fn ensure_object<'a>(
    map: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    let entry = map
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        bail!("YAML key `{key}` exists but is not a mapping");
    }
    Ok(entry.as_object_mut().expect("entry is object"))
}

fn ensure_string(map: &mut Map<String, Value>, key: &str, value: &str) {
    let missing_or_blank = map
        .get(key)
        .and_then(Value::as_str)
        .is_none_or(|existing| existing.trim().is_empty());
    if missing_or_blank {
        map.insert(key.to_owned(), json!(value));
    }
}

fn write_yaml_if_changed(path: &Path, value: &Value) -> Result<bool> {
    let serialized = serde_norway::to_string(value).context("serialize YAML")?;
    write_text_if_changed(path, &serialized)
}

fn write_json_if_changed(path: &Path, value: &Value) -> Result<bool> {
    let serialized = format!(
        "{}\n",
        serde_json::to_string_pretty(value).context("serialize JSON")?
    );
    write_text_if_changed(path, &serialized)
}

fn write_text_if_changed(path: &Path, content: &str) -> Result<bool> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(false);
    }
    fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_filigree_config(root: &Path, body: &str) {
        let dir = root.join(".weft/filigree");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), body).unwrap();
    }

    /// Server-mode Filigree fail-closes an unscoped federation write, so the
    /// bridge URL must carry the project scope `/api/p/{prefix}/…`.
    #[test]
    fn server_mode_filigree_yields_project_scoped_bridge_url() {
        let dir = tempfile::tempdir().unwrap();
        write_filigree_config(
            dir.path(),
            r#"{"prefix":"lacuna","name":"lacuna","mode":"server"}"#,
        );
        let desired = desired_bindings(dir.path());
        assert!(
            desired
                .wardline_filigree_url
                .ends_with("/api/p/lacuna/weft/scan-results"),
            "server-mode Filigree must scope the bridge URL: {}",
            desired.wardline_filigree_url
        );
    }

    /// Single-project (non-server) Filigree serves the unscoped `/api/…` mount,
    /// so the bridge URL stays unscoped.
    #[test]
    fn non_server_filigree_keeps_unscoped_bridge_url() {
        let dir = tempfile::tempdir().unwrap();
        write_filigree_config(
            dir.path(),
            r#"{"prefix":"lacuna","name":"lacuna","mode":"single"}"#,
        );
        let desired = desired_bindings(dir.path());
        assert!(
            desired
                .wardline_filigree_url
                .ends_with("/api/weft/scan-results")
                && !desired.wardline_filigree_url.contains("/api/p/"),
            "non-server Filigree keeps the unscoped path: {}",
            desired.wardline_filigree_url
        );
    }

    /// No Filigree config (Loomweave-solo, or pre-init) → unscoped, fail-soft.
    #[test]
    fn absent_filigree_config_keeps_unscoped_bridge_url() {
        let dir = tempfile::tempdir().unwrap();
        let desired = desired_bindings(dir.path());
        assert!(
            desired
                .wardline_filigree_url
                .ends_with("/api/weft/scan-results"),
            "absent Filigree config keeps the unscoped path: {}",
            desired.wardline_filigree_url
        );
    }

    /// A `.mcp.json` whose wardline entry matches what Loomweave would write
    /// except for the emit URL — same resolved command, same loomweave-url — so
    /// the emit URL is the only variable under test.
    fn mcp_json_matching(desired: &DesiredBindings, filigree_url: &str) -> String {
        let command = wardline_command();
        let loomweave_url = &desired.loomweave_url;
        format!(
            r#"{{"mcpServers":{{"wardline":{{"type":"stdio","command":"{command}","args":["mcp","--root",".","--loomweave-url","{loomweave_url}","--filigree-url","{filigree_url}"]}}}}}}"#
        )
    }

    /// The no-clobber guard: when Loomweave cannot itself scope the emit URL (no
    /// server-mode marker → `desired` is unscoped), it must NOT overwrite an
    /// existing scoped `--filigree-url`. Downgrading it to the unscoped form is
    /// what makes the server-mode daemon fail-close the write with a silent 400.
    #[test]
    fn install_preserves_existing_scoped_emit_url_when_unscoped_desired() {
        let dir = tempfile::tempdir().unwrap();
        // No Filigree config → desired_bindings yields an UNSCOPED url.
        let desired = desired_bindings(dir.path());
        assert!(
            !is_scoped_scan_url(&desired.wardline_filigree_url),
            "precondition: desired must be unscoped here"
        );
        let scoped = "http://127.0.0.1:8749/api/p/lacuna/weft/scan-results";
        fs::write(
            dir.path().join(".mcp.json"),
            mcp_json_matching(&desired, scoped),
        )
        .unwrap();

        // install must not change the file (scoped url preserved → entry identical).
        let changed = install_wardline_mcp(dir.path(), &desired).unwrap();
        assert!(!changed, "scoped emit URL must be preserved, not rewritten");

        let raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert!(
            raw.contains(scoped) && !raw.contains("/api/weft/scan-results"),
            "the scoped URL must survive; no unscoped downgrade:\n{raw}"
        );
        assert!(
            wardline_mcp_ok(dir.path(), &desired).unwrap(),
            "check must agree with write (no doctor flap)"
        );
    }

    /// When Loomweave CAN scope (server-mode marker present → `desired` scoped),
    /// its computed scoped value is authoritative and is written even over a
    /// stale scoped value.
    #[test]
    fn install_writes_scoped_when_desired_is_scoped() {
        let dir = tempfile::tempdir().unwrap();
        write_filigree_config(
            dir.path(),
            r#"{"prefix":"lacuna","name":"lacuna","mode":"server"}"#,
        );
        let desired = desired_bindings(dir.path());
        assert!(is_scoped_scan_url(&desired.wardline_filigree_url));
        // Pre-existing entry points at a *different* prefix; the scoped desired wins.
        fs::write(
            dir.path().join(".mcp.json"),
            mcp_json_matching(
                &desired,
                "http://127.0.0.1:8749/api/p/stale/weft/scan-results",
            ),
        )
        .unwrap();
        install_wardline_mcp(dir.path(), &desired).unwrap();
        let raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert!(
            raw.contains("/api/p/lacuna/weft/scan-results") && !raw.contains("/api/p/stale/"),
            "Loomweave's own scoped computation is authoritative:\n{raw}"
        );
    }
}
