//! `.claude/settings.json` SessionStart-hook merge.
//!
//! Merge semantics (never clobber): parse existing JSON, append a `SessionStart`
//! matcher-group running `clarion hook session-start --path "<project>"` only if
//! no existing `SessionStart` entry already runs `clarion hook session-start`
//! (regardless of its `--path`), and preserve every other key.
//!
//! Verified against the Claude Code settings schema: `hooks.SessionStart` is an
//! array of matcher-groups, each `{ "matcher"?, "hooks": [ {type,command} ] }`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// Substring that identifies Clarion's own `SessionStart` hook command.
pub const HOOK_COMMAND: &str = "clarion hook session-start";

/// Merge Clarion's `SessionStart` hook into a parsed settings `Value` in place,
/// inserting the supplied `command` (which must contain [`HOOK_COMMAND`] so the
/// idempotency predicate recognises it). Returns `true` if a change was made,
/// `false` if a clarion session-start hook was already present.
///
/// The idempotency predicate keys on the [`HOOK_COMMAND`] substring, so any
/// existing clarion session-start hook — regardless of its `--path` argument —
/// is detected and not duplicated.
#[must_use]
pub fn merge_session_start_hook(settings: &mut Value, command: &str) -> bool {
    // Coercion-after-parse: a successfully-parsed but malformed shape (a wrong
    // JSON type where we expect object/object/array) is rewritten to the
    // default shape rather than erroring. This is correct, but surface it so a
    // clobbered hand-authored shape is observable.
    let mut coerced = false;

    if !settings.is_object() {
        *settings = Value::Object(Map::new());
        coerced = true;
    }
    let obj = settings.as_object_mut().expect("settings is object");

    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
        coerced = true;
    }
    let hooks = hooks.as_object_mut().expect("hooks is object");

    let groups = hooks
        .entry("SessionStart")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !groups.is_array() {
        *groups = Value::Array(Vec::new());
        coerced = true;
    }
    let groups = groups.as_array_mut().expect("SessionStart is array");

    if coerced {
        tracing::warn!(
            "malformed .claude/settings.json shape (non-object settings/hooks or \
             non-array SessionStart) was rewritten to the expected shape before \
             merging the clarion SessionStart hook"
        );
    }

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
                "command": command
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

    // Never-clobber on the write path. `merge_session_start_hook` will happily
    // coerce a parseable-but-wrong-type shape (a top-level array, a non-object
    // `hooks`, a non-array `SessionStart`) to the default shape — fine for the
    // in-memory/unit-test callers, but on disk that would silently overwrite
    // hand-authored user content. Refuse to rewrite such a file; preserve it.
    if !settings.is_object() {
        bail!(
            "refusing to rewrite {}: top-level JSON is not an object (the file is \
             preserved unchanged). Fix or remove it, then re-run.",
            settings_path.display()
        );
    }
    if let Some(hooks) = settings.get("hooks") {
        if !hooks.is_object() {
            bail!(
                "refusing to rewrite {}: `hooks` is present but is not an object \
                 (the file is preserved unchanged). Fix or remove it, then re-run.",
                settings_path.display()
            );
        }
        if let Some(session_start) = hooks.get("SessionStart")
            && !session_start.is_array()
        {
            bail!(
                "refusing to rewrite {}: `hooks.SessionStart` is present but is not \
                 an array (the file is preserved unchanged). Fix or remove it, then \
                 re-run.",
                settings_path.display()
            );
        }
    }

    // Embed the resolved project path so the installed hook orients THIS
    // project no matter what working directory Claude Code runs it from.
    // `install::run` canonicalizes before calling, so `project_root` is already
    // absolute; canonicalize defensively in case another caller is not. The
    // path is shell-quoted because Claude runs hook commands via a shell.
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let command = format!(
        "clarion hook session-start --path \"{}\"",
        canonical.display()
    );

    let changed = merge_session_start_hook(&mut settings, &command);
    if !changed {
        return Ok(false);
    }

    fs::create_dir_all(&claude_dir).with_context(|| format!("mkdir {}", claude_dir.display()))?;
    let serialized =
        serde_json::to_string_pretty(&settings).context("serialize .claude/settings.json")?;

    // Atomic write: stage into a sibling temp file in the same directory, then
    // rename over the destination (same-filesystem atomic swap). This protects
    // the user's hand-authored settings.json from truncation/corruption on a
    // crash or concurrent install mid-write. Mirrors skill_pack::stage_and_swap.
    let tmp = claude_dir.join(format!(".settings.json.tmp-{}", std::process::id()));
    if let Err(err) = write_and_swap(&tmp, &settings_path, &serialized) {
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

    use serde_json::json;

    use super::{HOOK_COMMAND, install_session_start_hook, merge_session_start_hook};

    const TEST_COMMAND: &str = "clarion hook session-start --path \"/some/project\"";

    #[test]
    fn adds_hook_to_empty_settings() {
        let mut settings = json!({});
        let changed = merge_session_start_hook(&mut settings, TEST_COMMAND);
        assert!(changed, "should report a change");
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains(HOOK_COMMAND), "command was: {cmd}");
        assert!(cmd.contains("--path"), "command should pin --path: {cmd}");
        assert_eq!(groups[0]["hooks"][0]["type"], "command");
    }

    #[test]
    fn is_idempotent_when_hook_already_present() {
        let mut settings = json!({});
        assert!(merge_session_start_hook(&mut settings, TEST_COMMAND));
        // Second merge must be a no-op.
        assert!(!merge_session_start_hook(&mut settings, TEST_COMMAND));
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

        let changed = merge_session_start_hook(&mut settings, TEST_COMMAND);
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

    #[test]
    fn install_errors_on_unparseable_existing_settings() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(claude.join("settings.json"), "{not json").unwrap();

        let result = install_session_start_hook(dir.path());
        assert!(result.is_err(), "expected parse error, got {result:?}");
    }

    #[test]
    fn install_refuses_to_rewrite_top_level_non_object_settings() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        // Parseable JSON, but a top-level array — hand-authored user content we must not clobber.
        std::fs::write(claude.join("settings.json"), "[1, 2, 3]").unwrap();
        let result = super::install_session_start_hook(dir.path());
        assert!(
            result.is_err(),
            "should refuse to clobber a non-object settings.json"
        );
        // File must be untouched.
        let raw = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        assert_eq!(raw.trim(), "[1, 2, 3]");
    }

    #[test]
    fn install_refuses_to_rewrite_wrong_type_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("settings.json"),
            r#"{"hooks": "not-an-object"}"#,
        )
        .unwrap();
        let result = super::install_session_start_hook(dir.path());
        assert!(
            result.is_err(),
            "should refuse to clobber a wrong-type hooks value"
        );
    }

    #[test]
    fn install_refuses_to_rewrite_non_array_session_start() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("settings.json"),
            r#"{"hooks": {"SessionStart": "nope"}}"#,
        )
        .unwrap();
        let result = super::install_session_start_hook(dir.path());
        assert!(
            result.is_err(),
            "should refuse to clobber a non-array SessionStart value"
        );
        // File must be untouched.
        let raw = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        assert_eq!(raw.trim(), r#"{"hooks": {"SessionStart": "nope"}}"#);
    }

    #[test]
    fn installed_hook_command_embeds_resolved_project_path() {
        let dir = tempfile::tempdir().unwrap();
        super::install_session_start_hook(dir.path()).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let cmd = parsed["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains("clarion hook session-start"), "cmd: {cmd}");
        assert!(
            cmd.contains("--path"),
            "installed hook must pin --path: {cmd}"
        );
        // The path must reference this project's directory, not be path-less.
        let canon = dir.path().canonicalize().unwrap();
        assert!(
            cmd.contains(&canon.display().to_string()),
            "cmd should contain {} : {cmd}",
            canon.display()
        );
    }

    #[test]
    fn install_is_idempotent_on_disk() {
        let dir = tempfile::tempdir().unwrap();

        // First install writes and reports a change.
        assert!(install_session_start_hook(dir.path()).unwrap());
        // Second install is a no-op (no write, no change).
        assert!(!install_session_start_hook(dir.path()).unwrap());

        let raw = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert_eq!(
            raw.matches(HOOK_COMMAND).count(),
            1,
            "must contain exactly one hook entry; file was: {raw}"
        );
    }

    /// True when the filesystem enforces directory write permissions for this
    /// process (false as root, where DAC is bypassed). (clarion-86f4614c0b)
    #[cfg(unix)]
    fn perms_enforced() -> bool {
        use std::os::unix::fs::PermissionsExt;
        let probe = tempfile::tempdir().unwrap();
        let ro = probe.path().join("ro");
        fs::create_dir(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        fs::write(ro.join("probe"), b"x").is_err()
    }

    /// The atomic-write cleanup guard: when the staged write fails, the install
    /// must (a) surface the error, (b) leave the user's existing settings.json
    /// untouched, and (c) leak no `.settings.json.tmp-*` sibling. Triggered
    /// portably by making `.claude` read-only so the staged write fails with
    /// EACCES. (clarion-86f4614c0b)
    #[cfg(unix)]
    #[test]
    fn failed_install_preserves_settings_and_leaks_no_temp() {
        use std::os::unix::fs::PermissionsExt;

        if !perms_enforced() {
            eprintln!("skipping: directory permissions not enforced (running as root?)");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let claude_dir = dir.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        // Hand-authored settings WITHOUT the clarion hook, so the install will
        // try to add it (changed = true) and reach the staged write.
        let settings_path = claude_dir.join("settings.json");
        let original = "{\n  \"model\": \"opus\"\n}\n";
        fs::write(&settings_path, original).unwrap();

        // Make .claude read-only: the existing settings still reads (r-x), but
        // staging a temp file inside it fails.
        fs::set_permissions(&claude_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let result = install_session_start_hook(dir.path());

        // Inspect before restoring perms only where needed.
        let leaked: Vec<String> = fs::read_dir(&claude_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".settings.json.tmp-"))
            .collect();

        // Restore perms so tempdir cleanup succeeds.
        fs::set_permissions(&claude_dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_err(),
            "install into a read-only .claude must fail, not silently no-op"
        );
        assert!(
            leaked.is_empty(),
            "cleanup guard must leave no staging temp behind, found: {leaked:?}"
        );
        let after = fs::read_to_string(&settings_path).unwrap();
        assert_eq!(
            after, original,
            "a failed install must leave the user's settings.json byte-for-byte intact"
        );
    }
}
