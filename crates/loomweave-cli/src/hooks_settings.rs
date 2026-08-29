//! `.claude/settings.json` SessionStart-hook merge.
//!
//! Merge semantics (never clobber): parse existing JSON and ensure exactly one
//! `SessionStart` matcher-group runs `loomweave hook session-start --path
//! "${CLAUDE_PROJECT_DIR}"` — the host substitutes the variable at hook run
//! time, so the same tracked settings file serves every checkout and every
//! linked worktree. A functionally-equivalent existing entry (the templated
//! form, or a literal pin that resolves to this project root) is treated as
//! healthy and never rewritten. Loomweave-owned hooks are canonicalised — the
//! first is refreshed to the desired command and any extras (a stale
//! duplicate, or one pinned to a different project) are removed. Every other
//! key is preserved.
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
    /// Exactly one Loomweave hook is present and is functionally current for
    /// this project ([`hook_command_is_current`]).
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

/// The `SessionStart` hook command the installer writes: `--path` bound to
/// `${CLAUDE_PROJECT_DIR}`, which Claude Code substitutes when it runs the
/// hook. Never a baked absolute path — that silently points linked worktrees
/// at the main checkout and churns settings files tracked in git.
pub const DESIRED_HOOK_COMMAND: &str =
    r#"loomweave hook session-start --path "${CLAUDE_PROJECT_DIR}""#;

/// Whether an installed hook `command` is functionally current for
/// `project_root` — i.e. running it under this project would orient this
/// project. Current forms:
///
/// - `--path` bound to `$CLAUDE_PROJECT_DIR` / `${CLAUDE_PROJECT_DIR}`, bare
///   or double-quoted (forms the shell expands). Single-quoted is NOT current:
///   single quotes suppress expansion, so that entry is broken.
/// - `--path` a literal (bare or quoted) that resolves to this project root —
///   the pre-1.5.1 installer's baked-pin form.
///
/// Shared by the installer and the `doctor` state check so the two never
/// disagree on what "current" means. A current entry is left byte-for-byte
/// untouched; anything else is repaired to [`DESIRED_HOOK_COMMAND`].
#[must_use]
pub fn hook_command_is_current(command: &str, project_root: &Path) -> bool {
    let Some(rest) = command.strip_prefix(HOOK_COMMAND) else {
        return false;
    };
    let Some(arg) = rest.trim().strip_prefix("--path") else {
        return false;
    };
    let arg = arg.trim();
    if matches!(
        arg,
        "\"${CLAUDE_PROJECT_DIR}\""
            | "\"$CLAUDE_PROJECT_DIR\""
            | "${CLAUDE_PROJECT_DIR}"
            | "$CLAUDE_PROJECT_DIR"
    ) {
        return true;
    }
    // A literal pin: unwrap one layer of shell quoting and compare the path it
    // names against this project root (canonicalised on both sides, so a
    // symlinked spelling of the same directory still counts as current).
    let unquoted = arg
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| arg.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(arg);
    if unquoted.is_empty() || unquoted.contains('$') {
        return false;
    }
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    Path::new(unquoted) == canonical
        || Path::new(unquoted)
            .canonicalize()
            .is_ok_and(|p| p == canonical)
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
    } else if cmds.len() == 1 && hook_command_is_current(&cmds[0], project_root) {
        HookState::Present
    } else {
        HookState::Stale
    }
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

    // A functionally-current hook (templated ${CLAUDE_PROJECT_DIR} binding, or
    // a literal pin resolving to this project) must be left byte-for-byte
    // alone: settings.json is often tracked in git and partially owned by
    // other tools, so a no-op re-serialisation is still unsolicited churn.
    if session_start_hook_state(project_root) == HookState::Present {
        return Ok(false);
    }

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

    // Bind --path to ${CLAUDE_PROJECT_DIR} rather than baking the resolved
    // project path: Claude Code substitutes the variable when it runs the
    // hook, so the entry orients whichever checkout or linked worktree the
    // session actually opened — see `DESIRED_HOOK_COMMAND`.
    let changed = merge_session_start_hook(&mut settings, DESIRED_HOOK_COMMAND);
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
        HOOK_COMMAND, HookState, install_session_start_hook, loomweave_commands,
        merge_session_start_hook, session_start_hook_state,
    };

    const TEST_COMMAND: &str = "loomweave hook session-start --path \"/some/project\"";

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
    fn installed_hook_command_binds_claude_project_dir() {
        // The emitted command must be the portable templated form — the host
        // substitutes ${CLAUDE_PROJECT_DIR} at hook run time — never a baked
        // absolute path, which breaks tracked settings shared across checkouts
        // and points linked worktrees at the main checkout.
        let dir = tempfile::tempdir().unwrap();
        super::install_session_start_hook(dir.path()).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let cmd = parsed["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(
            cmd, r#"loomweave hook session-start --path "${CLAUDE_PROJECT_DIR}""#,
            "installed hook must bind --path to ${{CLAUDE_PROJECT_DIR}}"
        );
        let canon = dir.path().canonicalize().unwrap();
        assert!(
            !cmd.contains(&canon.display().to_string()),
            "installed hook must not bake the absolute project path: {cmd}"
        );
    }

    #[test]
    fn templated_claude_project_dir_hook_is_present_and_untouched() {
        // The elspeth regression: a committed hook already binding
        // ${CLAUDE_PROJECT_DIR} is functionally current. It must classify as
        // Present, and a re-install must leave the file byte-for-byte alone —
        // including foreign entries whose key order serde_json would otherwise
        // alphabetise on a rewrite.
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let original = concat!(
            "{\n",
            "  \"hooks\": {\n",
            "    \"SessionStart\": [\n",
            "      {\n",
            "        \"hooks\": [\n",
            "          {\n",
            "            \"type\": \"command\",\n",
            "            \"command\": \"legis session-context\",\n",
            "            \"timeout\": 5\n",
            "          },\n",
            "          {\n",
            "            \"type\": \"command\",\n",
            "            \"command\": \"loomweave hook session-start --path \\\"${CLAUDE_PROJECT_DIR}\\\"\"\n",
            "          }\n",
            "        ]\n",
            "      }\n",
            "    ]\n",
            "  }\n",
            "}\n"
        );
        fs::write(claude.join("settings.json"), original).unwrap();

        assert_eq!(
            session_start_hook_state(dir.path()),
            HookState::Present,
            "a ${{CLAUDE_PROJECT_DIR}}-templated hook is current, not stale"
        );
        let changed = install_session_start_hook(dir.path()).unwrap();
        assert!(!changed, "re-install must be a no-op on a templated hook");
        let after = fs::read_to_string(claude.join("settings.json")).unwrap();
        assert_eq!(after, original, "file must be byte-for-byte untouched");
    }

    #[test]
    fn legacy_absolute_pin_to_this_project_stays_present() {
        // The pre-fix installer baked the canonical project path. That form is
        // functionally current for this checkout, so it must not be churned on
        // the next install/doctor pass.
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let canon = dir.path().canonicalize().unwrap();
        let original = format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"loomweave hook session-start --path '{}'"}}]}}]}}}}"#,
            canon.display()
        );
        fs::write(claude.join("settings.json"), &original).unwrap();

        assert_eq!(
            session_start_hook_state(dir.path()),
            HookState::Present,
            "an absolute pin resolving to this project is current"
        );
        let changed = install_session_start_hook(dir.path()).unwrap();
        assert!(!changed, "re-install must not churn a current absolute pin");
        let after = fs::read_to_string(claude.join("settings.json")).unwrap();
        assert_eq!(after, original, "file must be byte-for-byte untouched");
    }

    #[test]
    fn single_quoted_templated_form_is_stale() {
        // Single quotes suppress shell expansion, so '--path
        // ${CLAUDE_PROJECT_DIR}' inside single quotes never resolves — that
        // entry is broken, not current, and must be repaired.
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join(".claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("settings.json"),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"loomweave hook session-start --path '${CLAUDE_PROJECT_DIR}'"}]}]}}"#,
        )
        .unwrap();
        assert_eq!(session_start_hook_state(dir.path()), HookState::Stale);
    }

    #[test]
    fn hook_command_is_current_accepts_equivalent_forms_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let canon = root.canonicalize().unwrap();
        let current = |arg: &str| format!("loomweave hook session-start --path {arg}");
        // Templated forms the shell expands: bare or double-quoted, braced or not.
        for arg in [
            "\"${CLAUDE_PROJECT_DIR}\"",
            "\"$CLAUDE_PROJECT_DIR\"",
            "${CLAUDE_PROJECT_DIR}",
            "$CLAUDE_PROJECT_DIR",
        ] {
            assert!(
                super::hook_command_is_current(&current(arg), root),
                "{arg} should be current"
            );
        }
        // Literal pins that resolve to this project root, in any quoting.
        for arg in [
            format!("'{}'", canon.display()),
            format!("\"{}\"", canon.display()),
            format!("{}", canon.display()),
        ] {
            assert!(
                super::hook_command_is_current(&current(&arg), root),
                "{arg} should be current"
            );
        }
        // Not current: single-quoted template (never expands), another project,
        // the path-less legacy form, a foreign command entirely.
        for cmd in [
            current("'${CLAUDE_PROJECT_DIR}'"),
            current("'/some/other/proj'"),
            "loomweave hook session-start".to_owned(),
            "legis session-context".to_owned(),
        ] {
            assert!(
                !super::hook_command_is_current(&cmd, root),
                "{cmd} should NOT be current"
            );
        }
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
