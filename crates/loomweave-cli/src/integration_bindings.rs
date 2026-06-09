//! Local three-way Loomweave/Filigree/Wardline dogfood bindings.
//!
//! These are intentionally configuration bindings, not a shared runtime:
//! Loomweave enables its own optional HTTP and Filigree read surfaces and, in
//! the project-local `.mcp.json`, registers the Wardline MCP server and stamps
//! **its own** `--loomweave-url` (the read-API Loomweave owns).
//!
//! Loomweave does **not** own Wardline's emit URL (`--filigree-url`): that is
//! project-scoped to the Filigree server-mode `/api/p/<prefix>/…` mount, which
//! Loomweave cannot compute, and an unscoped write fail-closes with a silent
//! 400 that drops every finding. So Loomweave carries an existing value forward
//! verbatim and otherwise omits the flag; **wardline's own installer owns and
//! scopes it** (ownership decision, weft emit incident 2026-06-10).
//!
//! Wardline receives URLs *only* via the `.mcp.json` launch flags (its
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

// Both fields are URLs by nature; the `_url` suffix is the meaningful part of
// each name, not redundant noise.
#[allow(clippy::struct_field_names)]
struct DesiredBindings {
    filigree_base_url: String,
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
    // NB: Loomweave no longer derives wardline's emit (`--filigree-url`) value —
    // that is wardline's installer's job, project-scoped to the server-mode
    // `/api/p/{prefix}/…` mount (ownership decision, weft emit incident
    // 2026-06-10). `filigree_base_url` here is only for Loomweave's OWN Filigree
    // *read* integration in loomweave.yaml, a separate path.
    //
    // ADR-044: seed the consumer's static target with this project's
    // deterministic read-API port. serve binds the same port (barring an
    // ephemeral fallback), and the published .weft/loomweave/ephemeral.port file
    // overrides this at runtime once a consumer resolves consume-time.
    let port = loomweave_federation::loomweave_port::deterministic_port(project_root);
    let loomweave_url = format!("http://127.0.0.1:{port}");
    DesiredBindings {
        filigree_base_url,
        loomweave_url,
    }
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
    // Loomweave cedes the emit URL: the expected args carry forward whatever
    // wardline recorded, so this check validates Loomweave's own fields
    // (command, --loomweave-url, structure) without judging wardline's emit URL.
    let existing = existing_wardline_filigree_url(&value);
    Ok(entry.get("args") == Some(&desired_wardline_args(desired, existing.as_deref())))
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
    // Read wardline's existing emit URL *before* mutating, so we carry it forward
    // verbatim (Loomweave cedes ownership; it never synthesizes or downgrades it).
    let existing = existing_wardline_filigree_url(&root);
    let root_obj = root.as_object_mut().expect("root is object");
    let servers = root_obj
        .entry("mcpServers".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers.as_object_mut().expect("mcpServers is object");
    let desired_entry = json!({
        "type": "stdio",
        "command": wardline_command(),
        "args": desired_wardline_args(desired, existing.as_deref()),
    });
    if servers.get("wardline") == Some(&desired_entry) {
        return Ok(false);
    }
    servers.insert("wardline".to_owned(), desired_entry);
    write_json_if_changed(&path, &root)
}

/// Loomweave does **not** own wardline's emit URL — wardline's own installer
/// writes and project-scopes `--filigree-url` (it knows its own Filigree prefix;
/// the server-mode daemon fail-closes an unscoped write with a silent 400 that
/// drops every finding). So Loomweave carries an existing value forward verbatim
/// and otherwise **omits** the flag; it never synthesizes one. (Ownership
/// decision, weft emit incident 2026-06-10: Loomweave cedes the emit URL to
/// wardline; it cannot compute the `/api/p/<prefix>/` scope itself.)
fn desired_wardline_args(desired: &DesiredBindings, filigree_url: Option<&str>) -> Value {
    let mut args = vec![
        json!("mcp"),
        json!("--root"),
        json!("."),
        json!("--loomweave-url"),
        json!(desired.loomweave_url),
    ];
    if let Some(url) = filigree_url {
        args.push(json!("--filigree-url"));
        args.push(json!(url));
    }
    Value::Array(args)
}

/// The `--filigree-url` value wardline currently records in the project's
/// `.mcp.json` entry, if any — carried forward verbatim (Loomweave never owns it).
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

    /// A `.mcp.json` wardline entry as wardline's installer would write it —
    /// resolved command, Loomweave's loomweave-url, and a wardline-owned scoped
    /// `--filigree-url`.
    fn wardline_entry_with_emit(desired: &DesiredBindings, filigree_url: &str) -> String {
        let command = wardline_command();
        let loomweave_url = &desired.loomweave_url;
        format!(
            r#"{{"mcpServers":{{"wardline":{{"type":"stdio","command":"{command}","args":["mcp","--root",".","--loomweave-url","{loomweave_url}","--filigree-url","{filigree_url}"]}}}}}}"#
        )
    }

    /// Cede: Loomweave carries wardline's existing scoped emit URL forward
    /// verbatim and does not rewrite the entry (the server-mode daemon
    /// fail-closes an unscoped write with a silent 400 — Loomweave must never
    /// downgrade or clobber the value wardline owns).
    #[test]
    fn install_carries_forward_existing_emit_url() {
        let dir = tempfile::tempdir().unwrap();
        let desired = desired_bindings(dir.path());
        let scoped = "http://127.0.0.1:8749/api/p/lacuna/weft/scan-results";
        fs::write(
            dir.path().join(".mcp.json"),
            wardline_entry_with_emit(&desired, scoped),
        )
        .unwrap();

        let changed = install_wardline_mcp(dir.path(), &desired).unwrap();
        assert!(
            !changed,
            "carried-forward emit URL must not rewrite the entry"
        );

        let raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert!(
            raw.contains(scoped) && !raw.contains("/api/weft/scan-results"),
            "wardline's scoped URL must survive verbatim; no downgrade:\n{raw}"
        );
        assert!(
            wardline_mcp_ok(dir.path(), &desired).unwrap(),
            "check agrees with write (no doctor flap)"
        );
    }

    /// Cede: on a fresh project Loomweave registers the wardline MCP server but
    /// writes NO `--filigree-url` — wardline's own installer owns and scopes that
    /// value. Loomweave must not synthesize a (necessarily unscoped → 400) URL.
    #[test]
    fn install_omits_emit_url_on_fresh_project() {
        let dir = tempfile::tempdir().unwrap();
        let desired = desired_bindings(dir.path());
        install_wardline_mcp(dir.path(), &desired).unwrap();

        let value: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        let args = value["mcpServers"]["wardline"]["args"]
            .as_array()
            .expect("wardline args present");
        assert!(
            args.iter().any(|a| a.as_str() == Some("--loomweave-url")),
            "Loomweave still owns --loomweave-url"
        );
        assert!(
            !args.iter().any(|a| a.as_str() == Some("--filigree-url")),
            "Loomweave must NOT write a --filigree-url on a fresh project: {args:?}"
        );
        assert!(
            wardline_mcp_ok(dir.path(), &desired).unwrap(),
            "the emit-URL-less entry Loomweave just wrote is its own definition of ok"
        );
    }

    /// Cede: Loomweave does not overwrite wardline's emit URL even when it
    /// differs from anything Loomweave would pick — it has no opinion on the
    /// value, only on its own fields.
    #[test]
    fn install_does_not_touch_wardline_emit_url_value() {
        let dir = tempfile::tempdir().unwrap();
        let desired = desired_bindings(dir.path());
        let arbitrary = "http://127.0.0.1:8749/api/p/some-other-prefix/weft/scan-results";
        fs::write(
            dir.path().join(".mcp.json"),
            wardline_entry_with_emit(&desired, arbitrary),
        )
        .unwrap();
        let changed = install_wardline_mcp(dir.path(), &desired).unwrap();
        assert!(!changed);
        let raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert!(
            raw.contains(arbitrary),
            "wardline's emit URL value is untouched by Loomweave:\n{raw}"
        );
    }
}
