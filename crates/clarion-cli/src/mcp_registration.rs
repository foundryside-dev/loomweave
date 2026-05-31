//! `.mcp.json` Clarion server-entry detection and never-clobber merge.
//!
//! `clarion install` does not register the MCP server today (it is a manual,
//! documented step), so `clarion doctor` is the surface that detects a missing
//! or mis-pointed `clarion` entry and — under `--fix` — repairs it.
//!
//! Merge semantics mirror [`crate::hooks_settings`]: parse the existing JSON,
//! touch only the `mcpServers.clarion` key, and preserve every other server
//! (e.g. a sibling `filigree` entry) and top-level key. A fresh entry uses the
//! bare `clarion` command (PATH-resolved, same convention as the `SessionStart`
//! hook); an existing entry keeps its `command` verbatim and only has its
//! `args` corrected to point at this project — never clobbering a deliberately
//! customised binary path.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// The `mcpServers` key Clarion owns.
pub const SERVER_KEY: &str = "clarion";

/// Read-only health of the `.mcp.json` Clarion registration, for `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpState {
    /// A `clarion` stdio server is registered and runs `serve --path <this
    /// project>`.
    Present,
    /// A `clarion` entry exists but does not target this project (wrong or
    /// missing `--path`, or not a `serve` invocation). Repairable in place.
    Stale,
    /// No `.mcp.json`, or it has no `clarion` server entry.
    Missing,
    /// `.mcp.json` exists but is not parseable JSON (or has a non-object shape).
    /// The merge refuses to clobber it, so this cannot be auto-repaired.
    Unparseable,
}

/// The `args` a `clarion` entry must carry to orient this project.
fn desired_args(project_root: &Path) -> Value {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    json!(["serve", "--path", canonical.display().to_string()])
}

/// True if `entry.args` runs `serve` with `--path <this project>`.
fn entry_targets_project(entry: &Value, project_root: &Path) -> bool {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let want = canonical.display().to_string();
    let Some(args) = entry.get("args").and_then(Value::as_array) else {
        return false;
    };
    let strs: Vec<&str> = args.iter().filter_map(Value::as_str).collect();
    let has_serve = strs.contains(&"serve");
    let path_ok = strs.windows(2).any(|w| w[0] == "--path" && w[1] == want);
    has_serve && path_ok
}

/// Classify the `.mcp.json` Clarion entry without writing anything.
#[must_use]
pub fn mcp_entry_state(project_root: &Path) -> McpState {
    let path = project_root.join(".mcp.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        return McpState::Missing;
    };
    if raw.trim().is_empty() {
        return McpState::Missing;
    }
    let Ok(root) = serde_json::from_str::<Value>(&raw) else {
        return McpState::Unparseable;
    };
    if !root.is_object() {
        return McpState::Unparseable;
    }
    match root.get("mcpServers") {
        Some(servers) if !servers.is_object() => return McpState::Unparseable,
        _ => {}
    }
    let Some(entry) = root.get("mcpServers").and_then(|m| m.get(SERVER_KEY)) else {
        return McpState::Missing;
    };
    if entry_targets_project(entry, project_root) {
        McpState::Present
    } else {
        McpState::Stale
    }
}

/// Read `.mcp.json` under `project_root` (creating `{}` if absent), merge
/// Clarion's `serve` entry, and write it back pretty-printed. Returns `true`
/// if the file changed.
///
/// Never-clobber: an existing `clarion` object entry keeps its `command`,
/// `type`, and `env`; only `args` are corrected. A fresh entry is written with
/// the bare `clarion` command. All other servers and top-level keys are
/// preserved.
///
/// # Errors
///
/// Returns an error if the existing file is present but unparseable, has a
/// non-object top level, or a non-object `mcpServers`, or if any read/write
/// fails. In those refuse-to-rewrite cases the file is left untouched.
pub fn install_mcp_entry(project_root: &Path) -> Result<bool> {
    let path = project_root.join(".mcp.json");

    let mut root: Value = if path.exists() {
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if raw.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?
        }
    } else {
        Value::Object(Map::new())
    };

    // Never-clobber: refuse to rewrite a hand-authored file with a shape we
    // don't expect, rather than coercing (and discarding) it.
    if !root.is_object() {
        bail!(
            "refusing to rewrite {}: top-level JSON is not an object (the file is \
             preserved unchanged). Fix or remove it, then re-run.",
            path.display()
        );
    }
    if let Some(servers) = root.get("mcpServers")
        && !servers.is_object()
    {
        bail!(
            "refusing to rewrite {}: `mcpServers` is present but is not an object \
             (the file is preserved unchanged). Fix or remove it, then re-run.",
            path.display()
        );
    }

    let want_args = desired_args(project_root);
    let obj = root.as_object_mut().expect("root is object");
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers.as_object_mut().expect("mcpServers is object");

    let changed = match servers.get_mut(SERVER_KEY) {
        // Existing object entry: preserve command/type/env, correct args only.
        Some(entry) if entry.is_object() => {
            let entry = entry.as_object_mut().expect("entry is object");
            if entry.get("args") == Some(&want_args) {
                false
            } else {
                entry.insert("args".to_string(), want_args);
                true
            }
        }
        // No entry (or a malformed non-object one we own): write a fresh entry
        // with the bare PATH-resolved command.
        _ => {
            servers.insert(
                SERVER_KEY.to_string(),
                json!({
                    "type": "stdio",
                    "command": "clarion",
                    "args": want_args,
                    "env": {},
                }),
            );
            true
        }
    };

    if !changed {
        return Ok(false);
    }

    let serialized = serde_json::to_string_pretty(&root).context("serialize .mcp.json")?;
    // Atomic write: stage a sibling temp file in the project root (same
    // filesystem), then rename over the destination. Mirrors
    // hooks_settings::write_and_swap.
    let tmp = project_root.join(format!(".mcp.json.tmp-{}", std::process::id()));
    if let Err(err) = write_and_swap(&tmp, &path, &serialized) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(true)
}

fn write_and_swap(tmp: &Path, dest: &Path, serialized: &str) -> Result<()> {
    fs::write(tmp, format!("{serialized}\n"))
        .with_context(|| format!("write staging {}", tmp.display()))?;
    fs::rename(tmp, dest)
        .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::{McpState, install_mcp_entry, mcp_entry_state};

    #[test]
    fn state_missing_then_present_around_install() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(mcp_entry_state(dir.path()), McpState::Missing);
        assert!(install_mcp_entry(dir.path()).unwrap());
        assert_eq!(mcp_entry_state(dir.path()), McpState::Present);
        // Idempotent: a second merge is a no-op.
        assert!(!install_mcp_entry(dir.path()).unwrap());
    }

    #[test]
    fn fresh_entry_uses_bare_command_and_pins_this_project() {
        let dir = tempfile::tempdir().unwrap();
        install_mcp_entry(dir.path()).unwrap();
        let raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let entry = &v["mcpServers"]["clarion"];
        assert_eq!(
            entry["command"], "clarion",
            "fresh entry must be PATH-resolved"
        );
        assert_eq!(entry["type"], "stdio");
        let canon = dir.path().canonicalize().unwrap().display().to_string();
        assert_eq!(
            entry["args"],
            serde_json::json!(["serve", "--path", canon]),
            "args must pin this project"
        );
    }

    #[test]
    fn install_preserves_other_servers_and_keeps_custom_command() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-existing file with a sibling server and a clarion entry that has a
        // deliberately customised command but a WRONG --path.
        fs::write(
            dir.path().join(".mcp.json"),
            r#"{
  "mcpServers": {
    "filigree": {"type": "stdio", "command": "/opt/filigree-mcp", "args": []},
    "clarion": {"type": "stdio", "command": "/custom/bin/clarion", "args": ["serve", "--path", "/old/proj"], "env": {}}
  }
}"#,
        )
        .unwrap();

        assert_eq!(
            mcp_entry_state(dir.path()),
            McpState::Stale,
            "wrong --path is Stale"
        );
        assert!(install_mcp_entry(dir.path()).unwrap());

        let v: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        // Sibling untouched.
        assert_eq!(v["mcpServers"]["filigree"]["command"], "/opt/filigree-mcp");
        // Custom command PRESERVED, args corrected to this project.
        assert_eq!(
            v["mcpServers"]["clarion"]["command"], "/custom/bin/clarion",
            "a customised command must be preserved, not clobbered"
        );
        let canon = dir.path().canonicalize().unwrap().display().to_string();
        assert_eq!(
            v["mcpServers"]["clarion"]["args"],
            serde_json::json!(["serve", "--path", canon])
        );
        assert_eq!(mcp_entry_state(dir.path()), McpState::Present);
    }

    #[test]
    fn unparseable_file_is_detected_and_install_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        fs::write(&path, "{not json").unwrap();
        assert_eq!(mcp_entry_state(dir.path()), McpState::Unparseable);
        let result = install_mcp_entry(dir.path());
        assert!(result.is_err(), "must refuse to clobber unparseable JSON");
        // File untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
    }

    #[test]
    fn install_refuses_non_object_mcpservers() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".mcp.json"), r#"{"mcpServers": "nope"}"#).unwrap();
        assert_eq!(mcp_entry_state(dir.path()), McpState::Unparseable);
        assert!(install_mcp_entry(dir.path()).is_err());
    }

    /// True when the filesystem enforces directory write permissions for this
    /// process (false as root, where DAC is bypassed).
    #[cfg(unix)]
    fn perms_enforced() -> bool {
        use std::os::unix::fs::PermissionsExt;
        let probe = tempfile::tempdir().unwrap();
        let ro = probe.path().join("ro");
        fs::create_dir(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        fs::write(ro.join("probe"), b"x").is_err()
    }

    /// A failed write must surface the error, leave any existing file intact,
    /// and leak no `.mcp.json.tmp-*` sibling.
    #[cfg(unix)]
    #[test]
    fn failed_write_preserves_file_and_leaks_no_temp() {
        use std::os::unix::fs::PermissionsExt;

        if !perms_enforced() {
            eprintln!("skipping: directory permissions not enforced (running as root?)");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        // Make the project root read-only so staging the temp file fails.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        let result = install_mcp_entry(dir.path());

        let leaked: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".mcp.json.tmp-"))
            .collect();

        // Restore perms so tempdir cleanup succeeds.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err(), "write into a read-only dir must fail");
        assert!(leaked.is_empty(), "leaked staging temp: {leaked:?}");
        assert!(
            !dir.path().join(".mcp.json").exists(),
            "no file should have been created"
        );
    }
}
