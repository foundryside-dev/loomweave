//! `.claude/settings.json` SessionStart-hook merge.
//!
//! Merge semantics (never clobber): parse existing JSON, append a `SessionStart`
//! matcher-group running `clarion hook session-start` only if no existing
//! `SessionStart` entry already runs that command, and preserve every other key.
//!
//! Verified against the Claude Code settings schema: `hooks.SessionStart` is an
//! array of matcher-groups, each `{ "matcher"?, "hooks": [ {type,command} ] }`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

/// Substring that identifies Clarion's own `SessionStart` hook command.
pub const HOOK_COMMAND: &str = "clarion hook session-start";

/// Merge Clarion's `SessionStart` hook into a parsed settings `Value` in place.
/// Returns `true` if a change was made, `false` if the hook was already present.
#[must_use]
pub fn merge_session_start_hook(settings: &mut Value) -> bool {
    if !settings.is_object() {
        *settings = Value::Object(Map::new());
    }
    let obj = settings.as_object_mut().expect("settings is object");

    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().expect("hooks is object");

    let groups = hooks
        .entry("SessionStart")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !groups.is_array() {
        *groups = Value::Array(Vec::new());
    }
    let groups = groups.as_array_mut().expect("SessionStart is array");

    let already_present = groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|inner| {
                inner.iter().any(|h| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains(HOOK_COMMAND))
                })
            })
    });
    if already_present {
        return false;
    }

    groups.push(json!({
        "hooks": [
            {
                "type": "command",
                "command": "clarion hook session-start"
            }
        ]
    }));
    true
}

/// Read `.claude/settings.json` under `project_root` (creating an empty object
/// if absent), merge Clarion's `SessionStart` hook, and write it back
/// pretty-printed. Returns `true` if the file changed.
///
/// # Errors
///
/// Returns an error if the existing file is present but unparseable, or if any
/// directory create / read / write fails.
pub fn install_session_start_hook(project_root: &Path) -> Result<bool> {
    let claude_dir = project_root.join(".claude");
    let settings_path = claude_dir.join("settings.json");

    let mut settings: Value = if settings_path.exists() {
        let raw = fs::read_to_string(&settings_path)
            .with_context(|| format!("read {}", settings_path.display()))?;
        if raw.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("parse {}", settings_path.display()))?
        }
    } else {
        Value::Object(Map::new())
    };

    let changed = merge_session_start_hook(&mut settings);
    if !changed {
        return Ok(false);
    }

    fs::create_dir_all(&claude_dir).with_context(|| format!("mkdir {}", claude_dir.display()))?;
    let serialized =
        serde_json::to_string_pretty(&settings).context("serialize .claude/settings.json")?;
    fs::write(&settings_path, format!("{serialized}\n"))
        .with_context(|| format!("write {}", settings_path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{HOOK_COMMAND, merge_session_start_hook};

    #[test]
    fn adds_hook_to_empty_settings() {
        let mut settings = json!({});
        let changed = merge_session_start_hook(&mut settings);
        assert!(changed, "should report a change");
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains(HOOK_COMMAND), "command was: {cmd}");
        assert_eq!(groups[0]["hooks"][0]["type"], "command");
    }

    #[test]
    fn is_idempotent_when_hook_already_present() {
        let mut settings = json!({});
        assert!(merge_session_start_hook(&mut settings));
        // Second merge must be a no-op.
        assert!(!merge_session_start_hook(&mut settings));
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "must not duplicate the hook");
    }

    #[test]
    fn preserves_unrelated_hooks_and_top_level_keys() {
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "echo bye"}]}
                ],
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "echo unrelated-greeting"}]}
                ]
            }
        });

        let changed = merge_session_start_hook(&mut settings);
        assert!(changed);

        assert_eq!(settings["model"], "opus");
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo bye"
        );
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "must append, not replace");
        let cmds: Vec<&str> = groups
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .map(|h| h["command"].as_str().unwrap())
            .collect();
        assert!(cmds.iter().any(|c| c.contains("unrelated-greeting")));
        assert!(cmds.iter().any(|c| c.contains(HOOK_COMMAND)));
    }
}
