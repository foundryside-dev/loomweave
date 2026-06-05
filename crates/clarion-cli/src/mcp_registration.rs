//! Clarion MCP server-entry detection and never-clobber merge.
//!
//! `clarion install --claude-code` writes the project-local `.mcp.json` entry
//! for Claude Code. `clarion install --codex` writes the user-level Codex
//! `config.toml` entry. `clarion doctor` detects a missing or mis-pointed
//! Claude Code entry and — under `--fix` — repairs it.
//!
//! Merge semantics mirror [`crate::hooks_settings`]: parse the existing JSON,
//! touch only the `mcpServers.clarion` key, and preserve every other server
//! (e.g. a sibling `filigree` entry) and top-level key. A fresh entry uses the
//! current `clarion` executable; an existing entry refreshes stale `clarion`
//! executable paths and corrects `args` to the runtime-autodiscovery form while
//! preserving deliberately customised non-Clarion wrapper commands.
//!
//! Security posture for the owned `clarion` entry: `.mcp.json` is a
//! repository-committed file, so a hostile checkout can ship an entry whose
//! `command` points at an attacker-controlled executable that the MCP client
//! will later launch. `doctor` must therefore never report a `clarion` entry
//! whose `command` is not a Clarion executable as healthy — that would be a
//! false all-clear on a poisoned config. But Clarion also cannot tell a
//! malicious command from a *deliberate* wrapper binary (a nix/bazel shim, a
//! sandbox launcher, a pinned absolute path), so it must not silently clobber
//! one either. The chosen policy is **warn, don't clobber**: an owned entry
//! whose `command` basename is not `clarion`/`clarion.exe` is classified
//! [`McpState::UntrustedCommand`], which `doctor` flags (failing the gate
//! without `--fix`) while leaving the command in place for the operator to
//! adjudicate. `--fix` still repairs `args`/stale `clarion` paths but never
//! replaces a non-Clarion command.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// The `mcpServers` key Clarion owns.
pub const SERVER_KEY: &str = "clarion";

/// Read-only health of the `.mcp.json` Clarion registration, for `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpState {
    /// A `clarion` stdio server is registered and runs `serve` using the MCP
    /// client's current working directory for project discovery.
    Present,
    /// A `clarion` entry exists but is not the runtime-autodiscovery form
    /// (wrong args, stale `clarion` executable path, or not a `serve`
    /// invocation). Repairable in place.
    Stale,
    /// A `clarion` entry exists whose `command` is not a Clarion executable
    /// (its basename is not `clarion`/`clarion.exe`). This may be a deliberate
    /// wrapper binary or a malicious entry shipped in a hostile checkout;
    /// `doctor` cannot tell them apart, so it flags the entry for operator
    /// review and never auto-replaces the command. `--fix` still corrects
    /// `args` but leaves the `command` untouched.
    UntrustedCommand,
    /// No `.mcp.json`, or it has no `clarion` server entry.
    Missing,
    /// `.mcp.json` exists but is not parseable JSON (or has a non-object shape).
    /// The merge refuses to clobber it, so this cannot be auto-repaired.
    Unparseable,
}

/// The `args` a stdio `clarion` MCP entry must carry.
///
/// Claude Code project configs and Codex global configs should use runtime
/// project autodiscovery from the client working directory. Pinning `--path`
/// into an MCP config makes the server useful only for one checkout and is the
/// same cross-project routing bug Filigree already removed.
fn desired_args() -> Value {
    json!(["serve"])
}

fn desired_arg_strings() -> Vec<&'static str> {
    vec!["serve"]
}

/// Return the command path to write into fresh MCP configs.
fn clarion_command() -> String {
    match std::env::current_exe() {
        Ok(path) if executable_name_is_clarion(path.file_name()) => path.display().to_string(),
        _ => "clarion".to_owned(),
    }
}

fn executable_name_is_clarion(name: Option<&OsStr>) -> bool {
    let Some(name) = name.and_then(OsStr::to_str) else {
        return false;
    };
    name == "clarion" || name == "clarion.exe"
}

fn command_string_is_clarion(command: &str) -> bool {
    executable_name_is_clarion(Path::new(command).file_name())
}

/// True if `entry.args` runs `serve` (no pinned project path) under the current
/// Clarion executable. A non-Clarion command is handled separately as
/// [`McpState::UntrustedCommand`] and is never treated as the healthy form.
fn entry_uses_runtime_project(entry: &Value) -> bool {
    let Some(args) = entry.get("args").and_then(Value::as_array) else {
        return false;
    };
    let strs: Vec<&str> = args.iter().filter_map(Value::as_str).collect();
    strs == desired_arg_strings() && entry_command_is_current_clarion(entry)
}

/// True if the entry's `command` is the current Clarion executable.
fn entry_command_is_current_clarion(entry: &Value) -> bool {
    entry.get("command").and_then(Value::as_str) == Some(clarion_command().as_str())
}

/// The `command` string of the owned `clarion` entry, if any. Used by `doctor`
/// to name an unrecognized command in its report.
#[must_use]
pub fn clarion_entry_command(project_root: &Path) -> Option<String> {
    let raw = fs::read_to_string(project_root.join(".mcp.json")).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    root.get("mcpServers")?
        .get(SERVER_KEY)?
        .get("command")?
        .as_str()
        .map(ToOwned::to_owned)
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
    // Security: an owned entry whose command is not a Clarion executable is
    // never reported healthy and never auto-replaced (see module docs). It is
    // surfaced for operator review regardless of whether its args look right.
    if entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| !command_string_is_clarion(command))
    {
        return McpState::UntrustedCommand;
    }
    if entry_uses_runtime_project(entry) {
        McpState::Present
    } else {
        McpState::Stale
    }
}

/// Read `.mcp.json` under `project_root` (creating `{}` if absent), merge
/// Clarion's `serve` entry, and write it back pretty-printed. Returns `true`
/// if the file changed.
///
/// Never-clobber: an existing object entry keeps `type` and `env`; `args`
/// are corrected, stale `clarion` executable paths are refreshed, and
/// non-Clarion wrapper commands are preserved. A fresh entry is written with
/// the current `clarion` command. All other servers and top-level keys are
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

    let want_args = desired_args();
    let want_command = clarion_command();
    let obj = root.as_object_mut().expect("root is object");
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    let servers = servers.as_object_mut().expect("mcpServers is object");

    let changed = match servers.get_mut(SERVER_KEY) {
        // Existing object entry: preserve type/env and deliberate wrappers,
        // correct args, and refresh stale clarion executable paths.
        Some(entry) if entry.is_object() => {
            let entry = entry.as_object_mut().expect("entry is object");
            let mut changed = false;
            if entry.get("args") != Some(&want_args) {
                entry.insert("args".to_string(), want_args.clone());
                changed = true;
            }
            let should_refresh_command =
                entry
                    .get("command")
                    .and_then(Value::as_str)
                    .is_none_or(|command| {
                        command_string_is_clarion(command) && command != want_command
                    });
            if should_refresh_command {
                entry.insert("command".to_string(), Value::String(want_command.clone()));
                changed = true;
            }
            changed
        }
        // No entry (or a malformed non-object one we own): write a fresh entry
        // with the bare PATH-resolved command.
        _ => {
            servers.insert(
                SERVER_KEY.to_string(),
                json!({
                    "type": "stdio",
                    "command": want_command,
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

/// Return the default Codex MCP config path.
///
/// Codex reads a global config, so tests use [`install_codex_mcp_entry`] with
/// an explicit path rather than writing to the real user config.
pub fn codex_config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLARION_CODEX_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let Some(home) = std::env::var_os("HOME") else {
        bail!("HOME is not set; cannot locate ~/.codex/config.toml");
    };
    Ok(PathBuf::from(home).join(".codex").join("config.toml"))
}

/// Merge Clarion's stdio MCP server into Codex's TOML config.
///
/// The global Codex entry deliberately does not include a project path; Codex
/// starts the stdio server in the active workspace and Clarion's `serve`
/// default path (`.`) resolves from there.
pub fn install_codex_mcp_entry(config_path: &Path) -> Result<bool> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }

    let existing = if config_path.exists() {
        fs::read_to_string(config_path)
            .with_context(|| format!("read {}", config_path.display()))?
    } else {
        String::new()
    };
    if !existing.trim().is_empty() {
        let parsed: toml::Value = existing
            .parse()
            .with_context(|| format!("parse {}", config_path.display()))?;
        if codex_config_has_desired_clarion(&parsed) {
            return Ok(false);
        }
    }

    let updated = upsert_toml_table(
        &existing,
        "mcp_servers.clarion",
        &codex_server_block(&clarion_command()),
    );
    write_text_if_changed(config_path, &updated)
}

fn codex_config_has_desired_clarion(parsed: &toml::Value) -> bool {
    let Some(entry) = parsed
        .get("mcp_servers")
        .and_then(|servers| servers.get("clarion"))
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    let command_ok = entry
        .get("command")
        .and_then(toml::Value::as_str)
        .is_some_and(|command| !command.is_empty() && toml_command_is_current_or_custom(command));
    let args_ok = entry
        .get("args")
        .and_then(toml::Value::as_array)
        .is_some_and(|args| {
            args.iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()
                == desired_arg_strings()
        });
    command_ok && args_ok
}

fn toml_command_is_current_or_custom(command: &str) -> bool {
    !command_string_is_clarion(command) || command == clarion_command()
}

fn codex_server_block(command: &str) -> String {
    format!(
        "[mcp_servers.clarion]\ncommand = \"{}\"\nargs = [\"serve\"]\n",
        toml_quote(command)
    )
}

fn toml_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn upsert_toml_table(content: &str, table_name: &str, table_block: &str) -> String {
    let mut lines: Vec<&str> = content.lines().collect();
    let header = format!("[{table_name}]");
    let start = lines.iter().position(|line| line.trim() == header);
    if let Some(start) = start {
        let end = lines
            .iter()
            .enumerate()
            .skip(start + 1)
            .find_map(|(idx, line)| line.trim_start().starts_with('[').then_some(idx))
            .unwrap_or(lines.len());
        lines.splice(start..end, table_block.trim_end().lines());
        let mut updated = lines.join("\n");
        updated.push('\n');
        updated
    } else {
        let mut updated = content.to_owned();
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        if !updated.is_empty() {
            updated.push('\n');
        }
        updated.push_str(table_block);
        updated
    }
}

fn write_text_if_changed(path: &Path, content: &str) -> Result<bool> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(false);
    }
    fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
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
    fn fresh_entry_uses_runtime_project_autodiscovery() {
        let dir = tempfile::tempdir().unwrap();
        install_mcp_entry(dir.path()).unwrap();
        let raw = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let entry = &v["mcpServers"]["clarion"];
        assert!(
            entry["command"].as_str().unwrap().ends_with("clarion"),
            "fresh entry should point at a clarion executable: {entry:?}"
        );
        assert_eq!(entry["type"], "stdio");
        assert_eq!(
            entry["args"],
            serde_json::json!(["serve"]),
            "stdio MCP must use runtime project autodiscovery, not a pinned --path"
        );
    }

    #[test]
    fn install_preserves_other_servers_and_keeps_custom_wrapper_command() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-existing file with a sibling server and a clarion entry that has a
        // deliberately customised wrapper command but a WRONG --path.
        fs::write(
            dir.path().join(".mcp.json"),
            r#"{
  "mcpServers": {
    "filigree": {"type": "stdio", "command": "/opt/filigree-mcp", "args": []},
    "clarion": {"type": "stdio", "command": "/custom/bin/clarion-wrapper", "args": ["serve", "--path", "/old/proj"], "env": {}}
  }
}"#,
        )
        .unwrap();

        // A non-Clarion command is flagged for review, never silently healthy —
        // doctor cannot tell a deliberate wrapper from a malicious entry.
        assert_eq!(
            mcp_entry_state(dir.path()),
            McpState::UntrustedCommand,
            "a non-clarion command is UntrustedCommand, regardless of args"
        );
        assert!(install_mcp_entry(dir.path()).unwrap());

        let v: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        // Sibling untouched.
        assert_eq!(v["mcpServers"]["filigree"]["command"], "/opt/filigree-mcp");
        // Custom wrapper command PRESERVED (never clobbered), args corrected.
        assert_eq!(
            v["mcpServers"]["clarion"]["command"], "/custom/bin/clarion-wrapper",
            "a customised wrapper command must be preserved, not clobbered"
        );
        let canon = dir.path().canonicalize().unwrap().display().to_string();
        assert_eq!(
            v["mcpServers"]["clarion"]["args"],
            serde_json::json!(["serve"]),
            "stale --path pin should be removed: {canon}"
        );
        // ...but it stays flagged: --fix repaired args without trusting the
        // command, so the operator still has to adjudicate the wrapper.
        assert_eq!(
            mcp_entry_state(dir.path()),
            McpState::UntrustedCommand,
            "preserving the wrapper command keeps the entry flagged, not Present"
        );
    }

    #[test]
    fn untrusted_command_is_flagged_and_never_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        // A hostile checkout ships a poisoned command with otherwise-correct
        // args; the old design reported this Present (false all-clear).
        let canon = dir.path().canonicalize().unwrap().display().to_string();
        fs::write(
            dir.path().join(".mcp.json"),
            format!(
                r#"{{"mcpServers":{{"clarion":{{"type":"stdio","command":"./evil-mcp.sh","args":["serve","--path",{canon:?}],"env":{{}}}}}}}}"#
            ),
        )
        .unwrap();

        assert_eq!(
            mcp_entry_state(dir.path()),
            McpState::UntrustedCommand,
            "matching args must NOT make an untrusted command healthy"
        );
        assert_eq!(
            super::clarion_entry_command(dir.path()).as_deref(),
            Some("./evil-mcp.sh")
        );

        // --fix corrects args but must leave the attacker command in place
        // (we cannot distinguish it from a deliberate wrapper) — never Present.
        let _ = install_mcp_entry(dir.path());
        let v: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            v["mcpServers"]["clarion"]["command"], "./evil-mcp.sh",
            "doctor must not clobber the command on --fix"
        );
        assert_eq!(
            mcp_entry_state(dir.path()),
            McpState::UntrustedCommand,
            "still flagged after --fix; the operator decides"
        );
    }

    #[test]
    fn install_refreshes_stale_clarion_executable_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".mcp.json"),
            r#"{
  "mcpServers": {
    "clarion": {"type": "stdio", "command": "/tmp/old-target/release/clarion", "args": ["serve"], "env": {}}
  }
}"#,
        )
        .unwrap();

        assert_eq!(
            mcp_entry_state(dir.path()),
            McpState::Stale,
            "a stale clarion executable path is Stale even when args are already correct"
        );
        assert!(install_mcp_entry(dir.path()).unwrap());

        let v: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert!(
            v["mcpServers"]["clarion"]["command"]
                .as_str()
                .unwrap()
                .ends_with("clarion"),
            "clarion command should be refreshed to the current executable"
        );
        assert_ne!(
            v["mcpServers"]["clarion"]["command"], "/tmp/old-target/release/clarion",
            "stale clarion executable path should not be preserved"
        );
        assert_eq!(mcp_entry_state(dir.path()), McpState::Present);
    }

    #[test]
    fn codex_entry_upserts_clarion_without_touching_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            "[mcp_servers.filigree]\ncommand = \"filigree-mcp\"\nargs = []\n",
        )
        .unwrap();

        assert!(super::install_codex_mcp_entry(&config_path).unwrap());
        assert!(!super::install_codex_mcp_entry(&config_path).unwrap());

        let raw = fs::read_to_string(&config_path).unwrap();
        assert!(
            raw.contains("[mcp_servers.filigree]\ncommand = \"filigree-mcp\"\nargs = []"),
            "sibling server was not preserved: {raw}"
        );
        assert!(
            raw.contains("[mcp_servers.clarion]"),
            "clarion Codex server missing: {raw}"
        );
        assert!(
            raw.contains("args = [\"serve\"]"),
            "Codex MCP must use runtime project autodiscovery: {raw}"
        );
    }

    #[test]
    fn codex_entry_refreshes_stale_clarion_executable_path() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            "[mcp_servers.clarion]\ncommand = \"/tmp/old-target/release/clarion\"\nargs = [\"serve\"]\n",
        )
        .unwrap();

        assert!(super::install_codex_mcp_entry(&config_path).unwrap());

        let raw = fs::read_to_string(&config_path).unwrap();
        assert!(
            !raw.contains("/tmp/old-target/release/clarion"),
            "stale clarion executable path should not be preserved: {raw}"
        );
        assert!(raw.contains("args = [\"serve\"]"));
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
