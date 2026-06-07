//! Local three-way Loomweave/Filigree/Wardline dogfood bindings.
//!
//! These are intentionally configuration bindings, not a shared runtime:
//! Loomweave enables its own optional HTTP and Filigree read surfaces, Wardline
//! receives the two peer URLs, and the project-local `.mcp.json` launches
//! Wardline with the same URLs for MCP scans.

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
        wardline_yaml_ok(project_root, &desired),
        wardline_mcp_ok(project_root, &desired),
    ) {
        (Ok(true), Ok(true), Ok(true)) => BindingState::Present,
        (Err(_), _, _) | (_, Err(_), _) | (_, _, Err(_)) => BindingState::Unparseable,
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
    changed |= install_wardline_yaml(project_root, &desired)?;
    changed |= install_wardline_mcp(project_root, &desired)?;
    Ok(changed)
}

fn desired_bindings(project_root: &Path) -> DesiredBindings {
    let filigree_base_url = live_filigree_base_url(project_root)
        .or_else(|| configured_filigree_base_url(project_root))
        .unwrap_or_else(|| DEFAULT_FILIGREE_BASE_URL.to_owned());
    let wardline_filigree_url = format!(
        "{}/api/weft/scan-results",
        filigree_base_url.trim_end_matches('/')
    );
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

fn wardline_yaml_ok(project_root: &Path, desired: &DesiredBindings) -> Result<bool> {
    let path = project_root.join("wardline.yaml");
    if !path.exists() {
        return Ok(false);
    }
    let value = read_yaml_value(&path)?;
    Ok(value
        .get("loomweave")
        .and_then(|loomweave| loomweave.get("url"))
        .and_then(Value::as_str)
        == Some(desired.loomweave_url.as_str())
        && value
            .get("filigree")
            .and_then(|filigree| filigree.get("url"))
            .and_then(Value::as_str)
            == Some(desired.wardline_filigree_url.as_str()))
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
    Ok(entry.get("args") == Some(&desired_wardline_args(desired)))
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

fn install_wardline_yaml(project_root: &Path, desired: &DesiredBindings) -> Result<bool> {
    let path = project_root.join("wardline.yaml");
    let mut value = read_yaml_value_or_empty(&path)?;
    let root = object_mut(&mut value, &path)?;
    let loomweave = ensure_object(root, "loomweave")?;
    loomweave.insert("url".to_owned(), json!(desired.loomweave_url));
    let filigree = ensure_object(root, "filigree")?;
    filigree.insert("url".to_owned(), json!(desired.wardline_filigree_url));
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
    let root_obj = root.as_object_mut().expect("root is object");
    let servers = root_obj
        .entry("mcpServers".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers.as_object_mut().expect("mcpServers is object");
    let desired_entry = json!({
        "type": "stdio",
        "command": wardline_command(),
        "args": desired_wardline_args(desired),
    });
    if servers.get("wardline") == Some(&desired_entry) {
        return Ok(false);
    }
    servers.insert("wardline".to_owned(), desired_entry);
    write_json_if_changed(&path, &root)
}

fn desired_wardline_args(desired: &DesiredBindings) -> Value {
    json!([
        "mcp",
        "--root",
        ".",
        "--loomweave-url",
        desired.loomweave_url,
        "--filigree-url",
        desired.wardline_filigree_url
    ])
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
