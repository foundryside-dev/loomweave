//! CLI-level tests for Task 4 of the worktree-index-isolation feature: config
//! precedence and sibling discovery routed through `WorktreeContext` (see
//! `docs/superpowers/specs/2026-07-18-loomweave-worktree-indexes-design.md`,
//! "Configuration and sibling discovery").
//!
//! Precedence under test, highest first: explicit `--config`, then
//! `<source-root>/loomweave.yaml`, then `<primary-root>/loomweave.yaml`, then
//! built-in defaults (write target: primary root). Sibling (Filigree)
//! local-state discovery and Loomweave's own port/instance-ID sidecar leaves
//! are tested directly against `loomweave_federation`/`loomweave_core::worktree`
//! — both are direct dependencies of this crate, so no CLI subprocess is
//! needed for those two.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use loomweave_core::worktree::WorktreeContext;
use loomweave_federation::config::FiligreeConfig;
use loomweave_federation::filigree_url::{dedup_candidate_roots, resolve_filigree_url_with_roots};
use loomweave_federation::loomweave_port::{
    publish_port_at, published_port_path, read_published_port_at,
};

fn loomweave_bin() -> Command {
    let mut cmd = Command::cargo_bin("loomweave").expect("loomweave binary");
    cmd.env(
        "LOOMWEAVE_CODEX_CONFIG",
        std::env::temp_dir().join(format!(
            "loomweave-test-codex-config-{}.toml",
            std::process::id()
        )),
    );
    cmd
}

fn git(dir: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?} in {}: {e}", dir.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
}

/// Build a primary repo plus one linked worktree, both with a real Git
/// working tree. No `loomweave install`/`analyze` — config-precedence and
/// sibling-discovery routing need only `.git`, not a `.weft/loomweave/`
/// store.
fn setup_primary_with_linked_worktree(
    root: &Path,
    worktree_name: &str,
    branch: &str,
) -> (PathBuf, PathBuf) {
    let repo = root.join("repo");
    init_repo(&repo);
    std::fs::write(repo.join("README.md"), "primary\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            &format!("../{worktree_name}"),
        ],
    );
    let linked = root.join(worktree_name);
    (repo, linked)
}

fn write_config(dir: &Path, contents: &str) {
    std::fs::write(dir.join("loomweave.yaml"), contents).unwrap();
}

const SOURCE_MARKER_YAML: &str = "version: 1\nllm_policy:\n  model_id: source-marker-model\n";
const PRIMARY_MARKER_YAML: &str = "version: 1\nllm_policy:\n  model_id: primary-marker-model\n";

/// Run `loomweave config <args>` with the given cwd and return
/// `(exit_code, stdout, stderr)`.
fn config_in(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let output = loomweave_bin()
        .arg("config")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run config");
    (
        output.status.code().expect("exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn source_root_config_wins_over_primary() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) =
        setup_primary_with_linked_worktree(tmp.path(), "cfg-source-wins", "feature-cfg-source");
    write_config(&repo, PRIMARY_MARKER_YAML);
    write_config(&linked, SOURCE_MARKER_YAML);

    let (code, stdout, stderr) = config_in(&linked, &["check"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let expected_path = linked.canonicalize().unwrap().join("loomweave.yaml");
    assert!(
        stdout.contains(&expected_path.display().to_string()),
        "expected the source-root loomweave.yaml path {expected_path:?} in output:\n{stdout}"
    );
    assert!(
        stdout.contains("source-marker-model"),
        "expected the source-root config's model_id, not the primary's: {stdout}"
    );
}

#[test]
fn primary_config_is_used_when_source_has_none() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        "cfg-primary-fallback",
        "feature-cfg-primary",
    );
    write_config(&repo, PRIMARY_MARKER_YAML);
    // No loomweave.yaml at the linked worktree's own root.

    let (code, stdout, stderr) = config_in(&linked, &["check"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let expected_path = repo.canonicalize().unwrap().join("loomweave.yaml");
    assert!(
        stdout.contains(&expected_path.display().to_string()),
        "expected the primary-root loomweave.yaml path {expected_path:?} in output:\n{stdout}"
    );
    assert!(
        stdout.contains("primary-marker-model"),
        "expected the primary-root config's model_id: {stdout}"
    );
}

#[test]
fn explicit_config_flag_wins_over_both() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) =
        setup_primary_with_linked_worktree(tmp.path(), "cfg-explicit-wins", "feature-cfg-explicit");
    write_config(&repo, PRIMARY_MARKER_YAML);
    write_config(&linked, SOURCE_MARKER_YAML);
    let explicit = tmp.path().join("explicit-loomweave.yaml");
    std::fs::write(
        &explicit,
        "version: 1\nllm_policy:\n  model_id: explicit-marker-model\n",
    )
    .unwrap();

    let (code, stdout, stderr) =
        config_in(&linked, &["check", "--config", explicit.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains(&explicit.display().to_string()),
        "expected the explicit --config path in output:\n{stdout}"
    );
    assert!(
        stdout.contains("explicit-marker-model"),
        "expected the explicit config's model_id, not source or primary: {stdout}"
    );
}

#[test]
fn llm_config_set_writes_the_resolved_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) =
        setup_primary_with_linked_worktree(tmp.path(), "cfg-llm-set", "feature-cfg-llm-set");
    write_config(&repo, PRIMARY_MARKER_YAML);
    write_config(&linked, SOURCE_MARKER_YAML);

    let (code, _stdout, stderr) = config_in(&linked, &["llm", "set", "--enable"]);
    assert_eq!(code, 0, "stderr: {stderr}");

    let source_yaml = std::fs::read_to_string(linked.join("loomweave.yaml")).unwrap();
    assert!(
        source_yaml.contains("enabled: true"),
        "the source-root loomweave.yaml (ConfigOrigin::Source) must be the one written: {source_yaml}"
    );
    let primary_yaml = std::fs::read_to_string(repo.join("loomweave.yaml")).unwrap();
    assert!(
        !primary_yaml.contains("enabled: true"),
        "the primary-root loomweave.yaml must be left untouched: {primary_yaml}"
    );
}

#[test]
fn semantic_config_set_writes_the_resolved_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        "cfg-semantic-set",
        "feature-cfg-semantic-set",
    );
    write_config(&repo, PRIMARY_MARKER_YAML);
    write_config(&linked, SOURCE_MARKER_YAML);

    let (code, _stdout, stderr) = config_in(&linked, &["semantic", "set", "--enable"]);
    assert_eq!(code, 0, "stderr: {stderr}");

    let source_yaml = std::fs::read_to_string(linked.join("loomweave.yaml")).unwrap();
    assert!(
        source_yaml.contains("semantic_search:") && source_yaml.contains("enabled: true"),
        "the source-root loomweave.yaml (ConfigOrigin::Source) must be the one written: {source_yaml}"
    );
    let primary_yaml = std::fs::read_to_string(repo.join("loomweave.yaml")).unwrap();
    assert_eq!(
        primary_yaml, PRIMARY_MARKER_YAML,
        "the primary-root loomweave.yaml must be left byte-for-byte untouched"
    );
}

#[test]
fn setter_creates_primary_target_when_no_file_existed() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        "cfg-default-target",
        "feature-cfg-default-target",
    );
    // Neither the source root nor the primary root has a loomweave.yaml yet.
    assert!(!repo.join("loomweave.yaml").exists());
    assert!(!linked.join("loomweave.yaml").exists());

    let (code, _stdout, stderr) = config_in(&linked, &["llm", "set", "--enable"]);
    assert_eq!(code, 0, "stderr: {stderr}");

    assert!(
        repo.join("loomweave.yaml").exists(),
        "ConfigOrigin::DefaultTarget must create the file at the PRIMARY root"
    );
    assert!(
        !linked.join("loomweave.yaml").exists(),
        "the linked worktree's own root must not get a loomweave.yaml when neither existed"
    );
    let created = std::fs::read_to_string(repo.join("loomweave.yaml")).unwrap();
    assert!(created.contains("enabled: true"), "created: {created}");
}

/// Sibling (Filigree) local-state discovery: source root first, then primary
/// root, deduplicated. Direct against `loomweave_federation` +
/// `WorktreeContext` (both direct dependencies of this crate) rather than a
/// full `loomweave serve` — see the Task 4 report for why.
#[test]
fn sibling_port_lookup_falls_back_source_then_primary() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) = setup_primary_with_linked_worktree(
        tmp.path(),
        "sibling-port-fallback",
        "feature-sibling-port",
    );
    let ctx = WorktreeContext::resolve(&linked).expect("resolve linked worktree context");
    assert_eq!(
        ctx.source_root.canonicalize().unwrap(),
        linked.canonicalize().unwrap()
    );
    assert_eq!(
        ctx.primary_root.canonicalize().unwrap(),
        repo.canonicalize().unwrap()
    );
    let roots = dedup_candidate_roots(&ctx.source_root, &ctx.primary_root);
    assert_eq!(
        roots.len(),
        2,
        "a linked worktree must yield two candidates"
    );

    let config = FiligreeConfig {
        enabled: true,
        ..FiligreeConfig::default()
    };

    // Case A: only the PRIMARY root has a live Filigree ephemeral.port — the
    // repository-wide instance. The source root has none, so the lookup must
    // fall back to the primary.
    let primary_filigree_dir = ctx.primary_root.join(".weft").join("filigree");
    std::fs::create_dir_all(&primary_filigree_dir).unwrap();
    std::fs::write(primary_filigree_dir.join("ephemeral.port"), "8542\n").unwrap();

    let resolution = resolve_filigree_url_with_roots(&config, &roots, |_| None);
    assert_eq!(
        resolution.resolved_url.as_deref(),
        Some("http://127.0.0.1:8542"),
        "must fall back to the primary root's published port when the source root has none"
    );

    // Case B: the SOURCE (worktree-local) root ALSO publishes a port — it
    // must win over the primary's.
    let source_filigree_dir = ctx.source_root.join(".weft").join("filigree");
    std::fs::create_dir_all(&source_filigree_dir).unwrap();
    std::fs::write(source_filigree_dir.join("ephemeral.port"), "8600\n").unwrap();

    let resolution = resolve_filigree_url_with_roots(&config, &roots, |_| None);
    assert_eq!(
        resolution.resolved_url.as_deref(),
        Some("http://127.0.0.1:8600"),
        "the source root's own published port must be preferred over the primary's"
    );
}

/// Loomweave's own port + instance-ID sidecar leaves must route through
/// `StorePaths` (the isolated worktree store), never re-derived from the
/// linked worktree's own (non-isolated) project root.
#[test]
fn own_port_and_instance_id_use_effective_store_leaves() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, linked) =
        setup_primary_with_linked_worktree(tmp.path(), "own-sidecar-leaves", "feature-own-sidecar");
    let ctx = WorktreeContext::resolve(&linked).expect("resolve linked worktree context");

    let expected_root = repo
        .canonicalize()
        .unwrap()
        .join(".weft/loomweave/worktrees")
        .join(
            ctx.stable_id
                .as_deref()
                .expect("linked worktree has a stable_id"),
        );
    assert_eq!(
        ctx.store_paths.instance_id,
        expected_root.join("instance_id"),
        "instance_id must live under the isolated worktree store, not the linked \
         worktree's own .weft/loomweave/"
    );
    assert_eq!(
        ctx.store_paths.port,
        expected_root.join("ephemeral.port"),
        "the port sidecar must live under the isolated worktree store"
    );

    // The OLD project-root-derived path (what a re-derivation from
    // `source_root` would produce) must be a DIFFERENT location, and
    // publishing through the new leaf-based function must never create
    // anything there.
    let old_derived_port_path = published_port_path(&linked);
    assert_ne!(
        old_derived_port_path, ctx.store_paths.port,
        "the isolated store's port leaf must differ from the linked worktree's own \
         project-root-derived path"
    );

    publish_port_at(&ctx.store_paths.port, 9555).expect("publish to the isolated store leaf");
    assert_eq!(read_published_port_at(&ctx.store_paths.port), Some(9555));
    assert!(
        !old_derived_port_path.exists(),
        "publishing to the isolated store leaf must not also create a port file under \
         the linked worktree's own .weft/loomweave/"
    );
}
