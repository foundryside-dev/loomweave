//! `.claude/settings.json` SessionStart-hook merge.
//!
//! Merge semantics (never clobber): parse existing JSON and ensure exactly one
//! `SessionStart` matcher-group runs `loomweave hook session-start --path
//! <project>` (the project path POSIX-single-quote-escaped for the shell).
//! Loomweave-owned hooks are canonicalised — the first is refreshed to the
//! desired command and any extras (a stale duplicate, or one pinned to a
//! different project) are removed. Every other key is preserved.
//!
//! Verified against the Claude Code settings schema: `hooks.SessionStart` is an
//! array of matcher-groups, each `{ "matcher"?, "hooks": [ {type,command} ] }`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

/// Substring that identifies Loomweave's own `SessionStart` hook command.
pub const HOOK_COMMAND: &str = "loomweave hook session-start";

/// Read-only health of the installed `SessionStart` hook, for `loomweave doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookState {
    /// Exactly one Loomweave hook is present and runs the command this project
    /// would install ([`desired_hook_command`]).
    Present,
    /// A Loomweave hook exists but is stale — the old path-less form, one pinned
    /// to a different project, or a duplicate. Repairable in place.
    Stale,
    /// No `.claude/settings.json`, or it has no Loomweave `SessionStart` hook.
    Missing,
    /// `.claude/settings.json` exists but is not parseable JSON. The merge
    /// refuses to clobber it, so this cannot be auto-repaired.
    Unparseable,
}

/// The `SessionStart` hook command this project would install: a bare
/// `loomweave hook session-start` (PATH-resolved) pinned to the absolute project
/// path (POSIX-single-quote-escaped). Shared by the installer and the
/// `doctor` state check so the two never disagree on what "current" means.
#[must_use]
pub fn desired_hook_command(project_root: &Path) -> String {
    // `install::run` canonicalizes before calling, so `project_root` is already
    // absolute; canonicalize defensively in case another caller is not.
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    format!(
        "loomweave hook session-start --path {}",
        shell_single_quote(&canonical.display().to_string())
    )
}

/// Every `command` string across all `SessionStart` groups that looks like a
/// Loomweave-owned hook (contains [`HOOK_COMMAND`]).
fn loomweave_commands(settings: &Value) -> Vec<String> {
    settings["hooks"]["SessionStart"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| g["hooks"].as_array())
        .flatten()
        .filter_map(|h| h["command"].as_str())
        .filter(|c| c.contains(HOOK_COMMAND))
        .map(str::to_owned)
        .collect()
}

/// Classify the installed `SessionStart` hook without writing anything, for
/// `loomweave doctor`. The repair for `Missing`/`Stale` is the idempotent
/// [`install_session_start_hook`]; `Unparseable` must be fixed by hand.
#[must_use]
pub fn session_start_hook_state(project_root: &Path) -> HookState {
    let settings_path = project_root.join(".claude").join("settings.json");
    let Ok(raw) = fs::read_to_string(&settings_path) else {
        return HookState::Missing;
    };
    if raw.trim().is_empty() {
        return HookState::Missing;
    }
    let Ok(settings) = serde_json::from_str::<Value>(&raw) else {
        return HookState::Unparseable;
    };
    let cmds = loomweave_commands(&settings);
    if cmds.is_empty() {
        HookState::Missing
    } else if cmds.len() == 1 && cmds[0] == desired_hook_command(project_root) {
        HookState::Present
    } else {
        HookState::Stale
    }
}

/// POSIX single-quote escaping for a value embedded in a shell command string.
///
/// Claude Code runs hook commands through a shell, so an embedded project path
/// must be a single literal argument — never word-split or subject to `$`,
/// backtick, or `\` expansion. Single quotes suppress all shell processing; the
/// only character that can't appear inside them is `'` itself, which we close
/// the quote for, emit an escaped `\'`, and reopen: `a'b` → `'a'\''b'`.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Merge Loomweave's `SessionStart` hook into a parsed settings `Value` in place,
/// inserting the supplied `command` (which must contain [`HOOK_COMMAND`] so it
/// is recognised as Loomweave-owned). Returns `true` if a change was made.
///
/// Loomweave-owned entries are keyed on the [`HOOK_COMMAND`] substring. The merge
/// canonicalises them to exactly one hook running `command`: the first is
/// refreshed and any extras (a stale duplicate, or a hook pinned to a different
/// project — possible in hand-merged settings) are removed, dropping any
/// Loomweave-dedicated group left empty. If none exists, the hook is appended.
/// Returns `false` only when a single Loomweave hook already runs `command` (the
/// idempotent re-install case); otherwise `true`.
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
             merging the loomweave SessionStart hook"
        );
    }

    // Locate every Loomweave-owned hook (its command contains HOOK_COMMAND),
    // across all matcher-groups. Pass 1 only reads, so the immutable borrow is
    // released before any mutation below.
    let mut locations: Vec<(usize, usize)> = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        let Some(inner) = group.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        for (hi, h) in inner.iter().enumerate() {
            if h.get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| c.contains(HOOK_COMMAND))
            {
                locations.push((gi, hi));
            }
        }
    }

    if locations.is_empty() {
        groups.push(json!({
            "hooks": [
                {
                    "type": "command",
                    "command": command
                }
            ]
        }));
        return true;
    }

    // Canonicalise to exactly one Loomweave hook running `command`: refresh the
    // first, then remove any extras (a stale duplicate, or a hook pinned to a
    // different project — e.g. hand-merged settings). This delivers "don't
    // no-op on a stale hook, don't leave duplicates, don't silently keep a
    // wrong-project pin" even when a current and a stale entry coexist or
    // multiple stale entries exist. Returns `false` only when a single Loomweave
    // hook already runs `command` (the idempotent re-install case).
    let mut changed = false;
    let (kg, kh) = locations[0];
    if groups[kg]["hooks"][kh]["command"].as_str() != Some(command) {
        groups[kg]["hooks"][kh]["command"] = Value::String(command.to_string());
        changed = true;
    }

    // Remove the extras. Descending order keeps inner indices valid as we go.
    let mut extras: Vec<(usize, usize)> = locations[1..].to_vec();
    extras.sort_unstable_by(|a, b| b.cmp(a));
    let mut touched_groups = std::collections::BTreeSet::new();
    for (gi, hi) in extras {
        if let Some(inner) = groups[gi]["hooks"].as_array_mut() {
            inner.remove(hi);
            touched_groups.insert(gi);
            changed = true;
        }
    }
    // Drop any Loomweave-dedicated group we just emptied (descending to keep
    // indices valid). A group still holding unrelated hooks is left intact.
    for gi in touched_groups.into_iter().rev() {
        if groups[gi]["hooks"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty)
        {
            groups.remove(gi);
        }
    }
    changed
}

/// Read `.claude/settings.json` under `project_root` (creating an empty object
/// if absent), merge Loomweave's `SessionStart` hook, and write it back
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
    // project no matter what working directory Claude Code runs it from. The
    // path is POSIX-single-quote-escaped (not merely double-quoted) because
    // Claude runs hook commands via a shell and the path may contain `$`,
    // backticks, quotes, or backslashes — see `desired_hook_command`.
    let command = desired_hook_command(project_root);

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

    use super::{
        HOOK_COMMAND, HookState, loomweave_commands, install_session_start_hook,
        merge_session_start_hook, session_start_hook_state,
    };

    const TEST_COMMAND: &str = "loomweave hook session-start --path \"/some/project\"";

    #[cfg(unix)]
    fn sh_roundtrip(quoted: &str) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .expect("run sh");
        String::from_utf8(out.stdout).expect("utf8")
    }

    #[cfg(unix)]
    #[test]
    fn shell_quote_round_trips_metacharacters_through_a_real_shell() {
        // The installed hook command is run by Claude through a shell. A path
        // with shell metacharacters must survive as a single literal argument,
        // never expanded or split. Double-quote wrapping (the prior form) lets
        // $, backtick, and \ act; single-quote escaping does not. Prove the
        // helper round-trips through `sh` exactly. (loomweave review #5)
        for s in [
            "/plain/path",
            "/with space/x",
            "/we'ird/x",
            "/$(touch pwned)/x",
            "/back`tick`/x",
            "/back\\slash/x",
            "/dquote\"here/x",
            "/a'b\"c$d`e/x",
            "", // the one structurally distinct (zero-iteration) input
        ] {
            assert_eq!(
                sh_roundtrip(&super::shell_single_quote(s)),
                s,
                "shell_single_quote did not round-trip {s:?} through sh"
            );
        }
        // The empty string must produce a valid empty shell word, not nothing.
        assert_eq!(super::shell_single_quote(""), "''");
    }

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
    fn refreshes_a_stale_loomweave_hook_in_place() {
        // A previously-installed Loomweave hook (e.g. the old path-less form, or
        // one pinned to a different project) must be refreshed to the desired
        // command on re-install, not left stale. The idempotency check keys on
        // the HOOK_COMMAND substring, so a stale entry used to no-op forever.
        // (loomweave review #10)
        let mut settings = json!({
            "hooks": {"SessionStart": [
                {"hooks": [{"type": "command", "command": "loomweave hook session-start"}]}
            ]}
        });
        let desired = "loomweave hook session-start --path '/proj'";
        let changed = merge_session_start_hook(&mut settings, desired);
        assert!(changed, "a stale Loomweave hook must be refreshed");
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            groups.len(),
            1,
            "must refresh in place, not append a duplicate"
        );
        assert_eq!(
            groups[0]["hooks"][0]["command"].as_str().unwrap(),
            desired,
            "stale hook command must be updated to the desired command"
        );
        // And a second merge with the now-current command is a no-op.
        assert!(
            !merge_session_start_hook(&mut settings, desired),
            "re-merging the current command must be a no-op"
        );
    }

    #[test]
    fn refreshes_a_stale_hook_pinned_to_a_different_path() {
        // The realistic re-install case: the repo moved, so the existing hook
        // pins the old project path. It must be refreshed to the new path, not
        // no-oped. (loomweave review #4/#10)
        let mut settings = json!({
            "hooks": {"SessionStart": [
                {"hooks": [{"type": "command",
                    "command": "loomweave hook session-start --path '/old/proj'"}]}
            ]}
        });
        let desired = "loomweave hook session-start --path '/new/proj'";
        assert!(merge_session_start_hook(&mut settings, desired));
        assert_eq!(loomweave_commands(&settings), vec![desired.to_string()]);
    }

    #[test]
    fn removes_a_stale_hook_when_a_current_one_already_exists() {
        // A current hook coexisting with a stale one pinned to a different
        // project (e.g. hand-merged settings). The stale one silently orients
        // the wrong project every session, so it must be reconciled away —
        // leaving exactly one Loomweave hook running the desired command.
        // (loomweave review #10 — found_current must not short-circuit the sweep)
        let desired = "loomweave hook session-start --path '/proj'";
        let mut settings = json!({
            "hooks": {"SessionStart": [
                {"hooks": [{"type": "command", "command": desired}]},
                {"hooks": [{"type": "command",
                    "command": "loomweave hook session-start --path '/other'"}]}
            ]}
        });
        assert!(
            merge_session_start_hook(&mut settings, desired),
            "a stale entry coexisting with the current one must be reconciled"
        );
        assert_eq!(
            loomweave_commands(&settings),
            vec![desired.to_string()],
            "exactly one Loomweave hook must remain, running the desired command"
        );
    }

    #[test]
    fn dedups_multiple_stale_loomweave_hooks() {
        // Two stale Loomweave hooks, no current one. Must converge to a single
        // hook running the desired command, not leave survivors. (loomweave #10)
        let desired = "loomweave hook session-start --path '/proj'";
        let mut settings = json!({
            "hooks": {"SessionStart": [
                {"hooks": [{"type": "command", "command": "loomweave hook session-start"}]},
                {"hooks": [{"type": "command",
                    "command": "loomweave hook session-start --path '/old'"}]}
            ]}
        });
        assert!(merge_session_start_hook(&mut settings, desired));
        assert_eq!(loomweave_commands(&settings), vec![desired.to_string()]);
        // Convergent: a second merge is now a no-op.
        assert!(!merge_session_start_hook(&mut settings, desired));
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
    fn hook_state_missing_then_present_around_install() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            session_start_hook_state(dir.path()),
            HookState::Missing,
            "no settings.json -> Missing"
        );
        install_session_start_hook(dir.path()).unwrap();
        assert_eq!(
            session_start_hook_state(dir.path()),
            HookState::Present,
            "a fresh install is Present"
        );
    }

    #[test]
    fn hook_state_stale_when_pinned_to_a_different_path() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        // A Loomweave hook pinned to some other project: present-but-wrong.
        fs::write(
            claude.join("settings.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"loomweave hook session-start --path '/some/other/proj'"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(session_start_hook_state(dir.path()), HookState::Stale);
    }

    #[test]
    fn hook_state_unparseable_on_bad_json() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(claude.join("settings.json"), "{not json").unwrap();
        assert_eq!(session_start_hook_state(dir.path()), HookState::Unparseable);
    }

    #[test]
    fn hook_state_missing_when_only_unrelated_hooks_present() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("settings.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(
            session_start_hook_state(dir.path()),
            HookState::Missing,
            "an unrelated SessionStart hook is not a Loomweave hook"
        );
        // And loomweave_commands sees nothing Loomweave-owned here.
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(claude.join("settings.json")).unwrap())
                .unwrap();
        assert!(loomweave_commands(&settings).is_empty());
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
        assert!(cmd.contains("loomweave hook session-start"), "cmd: {cmd}");
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
        // Hand-authored settings WITHOUT the loomweave hook, so the install will
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
