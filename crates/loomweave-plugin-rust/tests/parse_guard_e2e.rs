//! Parse-guard integration tests (ADR-050 §plugin-guards): hostile sources —
//! bracket-depth bombs, unary prefix bombs, oversize files — must DEGRADE to a
//! single `module` entity plus one warning finding, never abort the plugin.
//! Every hostile source is GENERATED in-test; nothing hostile is checked in.
//!
//! Drives the real `loomweave-plugin-rust` subprocess through
//! [`PluginHost::spawn`], exactly like `host_integration.rs` (the harness
//! helpers are copied from there): `spawn` runs the full handshake, and the
//! plugin builds its init-time symbol table over the WHOLE project during
//! `initialize` — so a successful spawn over a bomb-bearing project is itself
//! proof the symbol-table walk skips bombs instead of overflowing the stack.

use loomweave_core::PluginHost;
use loomweave_core::plugin::parse_manifest;

/// This crate's shipped manifest — embedded at compile time.
const RUST_MANIFEST_BYTES: &[u8] = include_bytes!("../plugin.toml");

/// The concrete subprocess-backed host type `PluginHost::spawn` returns.
type SubprocessHost = PluginHost<
    std::io::BufReader<std::process::ChildStdout>,
    std::io::BufWriter<std::process::ChildStdin>,
>;

// ── harness (copied from host_integration.rs) ────────────────────────────────

/// Locate the off-glob `loomweave-rust-plugin` binary in the Cargo target dir,
/// building it on demand if missing.
fn rust_plugin_binary_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_loomweave-rust-plugin") {
        return std::path::PathBuf::from(path);
    }

    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/loomweave-plugin-rust -> crates -> workspace root
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must exist");

    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map_or_else(|_| workspace_root.join("target"), std::path::PathBuf::from);

    if let Some(path) = find_rust_plugin_binary(&target_dir) {
        return path;
    }

    build_rust_plugin_binary(workspace_root, &target_dir);

    if let Some(path) = find_rust_plugin_binary(&target_dir) {
        return path;
    }

    panic!(
        "loomweave-rust-plugin binary not found. \
         Tried `cargo build -p loomweave-plugin-rust --bin loomweave-rust-plugin`. \
         Searched in: {}",
        target_dir.display()
    );
}

fn find_rust_plugin_binary(target_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for profile in &["debug", "release"] {
        let candidate = target_dir.join(profile).join(format!(
            "loomweave-rust-plugin{}",
            std::env::consts::EXE_SUFFIX
        ));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn build_rust_plugin_binary(workspace_root: &std::path::Path, target_dir: &std::path::Path) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = std::process::Command::new(cargo)
        .current_dir(workspace_root)
        .arg("build")
        .arg("-p")
        .arg("loomweave-plugin-rust")
        .arg("--bin")
        .arg("loomweave-rust-plugin")
        .arg("--target-dir")
        .arg(target_dir)
        .output()
        .expect("spawn cargo build for loomweave-plugin-rust");

    assert!(
        output.status.success(),
        "cargo build for loomweave-plugin-rust failed with status {}.\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Stage the off-glob binary under its manifest-declared basename
/// (`loomweave-plugin-rust`) and return `(tempdir, staged_path)`. Keep the
/// `TempDir` alive for the duration of the spawn.
fn staged_rust_plugin() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("create staging dir");
    let staged = dir.path().join(format!(
        "loomweave-plugin-rust{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(rust_plugin_binary_path(), &staged).expect("stage rust plugin binary");
    // Fence the copy against ETXTBSY (matching host_subprocess.rs): under a
    // saturated `--workspace` run the kernel can still consider the
    // freshly-written image "busy" when we exec it, because a writer fd from the
    // copy may not have fully settled across threads. Open-sync-close the staged
    // file so its data is durable and no writable handle survives into the exec;
    // `spawn_over` additionally retries the exec for the residual window.
    #[cfg(unix)]
    {
        let f = std::fs::File::open(&staged).expect("reopen staged plugin for sync");
        f.sync_all().expect("sync staged plugin to disk");
        drop(f);
    }
    (dir, staged)
}

// ── generators (hostile sources are generated, never checked in) ─────────────

/// `pub fn f() { let _ = (((…1…))); }` — the bracket bomb. Pre-guard, depth
/// 3000 aborted (SIGABRT) the plugin (syn first-crash at parens ≈760 on 8 MiB).
fn deep_paren_bomb(n: usize) -> String {
    format!(
        "pub fn f() {{ let _ = {}1{}; }}\n",
        "(".repeat(n),
        ")".repeat(n)
    )
}

/// `pub fn f() { let _ = !!!…1; }` — the unary prefix bomb (bracket depth ~1,
/// invisible to a pure depth scan; the prefix-run cap catches it).
fn unary_bomb(n: usize) -> String {
    format!("pub fn f() {{ let _ = {}1; }}\n", "!".repeat(n))
}

/// Valid Rust, ~1 KiB per fn, repeated until just over `target_bytes`.
fn oversize_source(target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes + 2048);
    let filler = "x".repeat(900);
    let mut i = 0usize;
    while s.len() <= target_bytes {
        use std::fmt::Write as _;
        writeln!(s, "pub fn filler_{i}() {{ let _ = \"{filler}\"; }}").expect("write to string");
        i += 1;
    }
    s
}

/// Lay out a one-crate project (`name = "bombproj"`) with the given
/// `src/`-relative files, plus a generated `lib.rs` declaring each as a `pub
/// mod`. Returns `(project_tempdir, project_root)`.
fn staged_project(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let project_dir = tempfile::TempDir::new().expect("create project dir");
    let root = project_dir.path().to_path_buf();
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"bombproj\"\n")
        .expect("write Cargo.toml");
    let mut lib = String::new();
    for (name, content) in files {
        use std::fmt::Write as _;
        let module = name.strip_suffix(".rs").expect("module file name");
        writeln!(lib, "pub mod {module};").expect("write to string");
        std::fs::write(src.join(name), content).expect("write module file");
    }
    std::fs::write(src.join("lib.rs"), lib).expect("write lib.rs");
    (project_dir, root)
}

/// Spawn the real plugin over `root`. A successful return is the
/// no-abort-at-initialize proof (the symbol-table walk runs inside the
/// handshake).
fn spawn_over(root: &std::path::Path) -> (tempfile::TempDir, SubprocessHost, std::process::Child) {
    let manifest = parse_manifest(RUST_MANIFEST_BYTES).expect("rust plugin.toml must be valid");
    let (binary_stage, exec) = staged_rust_plugin();
    // Retry the exec briefly on ETXTBSY: copying an executable and immediately
    // exec'ing it can race the kernel's "text file busy" guard under heavy
    // parallel load — a pure test-staging artifact (production PluginHost::spawn
    // is unchanged). Matches the spawn_staged_with_retry fence in
    // host_subprocess.rs so this suite is deterministic under `--workspace`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let (host, child) = loop {
        match PluginHost::spawn(manifest.clone(), root, &exec) {
            Err(loomweave_core::HostError::Spawn(msg))
                if msg.contains("Text file busy")
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => {
                break other
                    .expect("spawn must succeed: the symbol-table walk must skip bombs, not abort");
            }
        }
    };
    (binary_stage, host, child)
}

/// Assert the degraded shape for one analyzed bomb: exactly one `module`
/// entity carrying `parse_status == expected_status`, and exactly one
/// host-accepted finding with `subcode == expected_subcode`, severity warning.
fn assert_degraded(
    host: &mut SubprocessHost,
    file: &std::path::Path,
    expected_status: &str,
    expected_subcode: &str,
) {
    let outcome = host.analyze_file(file).expect("analyze_file must succeed");
    assert_eq!(
        outcome.entities.len(),
        1,
        "expected exactly one degraded entity; got {:?}",
        outcome
            .entities
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>()
    );
    let entity = &outcome.entities[0];
    assert_eq!(entity.kind, "module", "degraded entity must be a module");
    assert_eq!(
        entity
            .raw
            .extra
            .get("parse_status")
            .and_then(|v| v.as_str()),
        Some(expected_status),
        "degraded module must carry parse_status={expected_status}; extra={:?}",
        entity.raw.extra
    );
    assert!(
        outcome.edges.is_empty(),
        "a degraded file contributes no edges; got {:?}",
        outcome.edges
    );
    let findings = host.take_findings();
    let matching: Vec<_> = findings
        .iter()
        .filter(|f| f.subcode == expected_subcode)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one {expected_subcode} finding; got {findings:?}"
    );
    assert_eq!(
        matching[0].metadata.get("severity").map(String::as_str),
        Some("warning"),
        "guard finding must be a warning; got {:?}",
        matching[0]
    );
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn depth_bomb_degrades_to_finding_not_crash() {
    let (_project, root) = staged_project(&[
        ("bomb.rs", &deep_paren_bomb(3000)),
        ("ok.rs", "pub fn fine() {}\n"),
    ]);
    let (_stage, mut host, mut child) = spawn_over(&root);

    assert_degraded(
        &mut host,
        &root.join("src/bomb.rs"),
        "depth_limit",
        "LMWV-RUST-DEPTH-LIMIT",
    );

    // The benign sibling still extracts normally.
    let ok = host
        .analyze_file(&root.join("src/ok.rs"))
        .expect("benign file must analyze");
    assert!(
        ok.entities
            .iter()
            .any(|e| e.id.as_str() == "rust:function:bombproj.ok.fine"),
        "benign sibling must extract normally; got {:?}",
        ok.entities
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>()
    );

    host.shutdown().expect("shutdown must succeed");
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "plugin must exit 0; got {status:?}");
}

#[test]
fn unary_bomb_degrades() {
    let (_project, root) = staged_project(&[("bomb.rs", &unary_bomb(4000))]);
    let (_stage, mut host, mut child) = spawn_over(&root);

    assert_degraded(
        &mut host,
        &root.join("src/bomb.rs"),
        "depth_limit",
        "LMWV-RUST-DEPTH-LIMIT",
    );

    host.shutdown().expect("shutdown must succeed");
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "plugin must exit 0; got {status:?}");
}

#[test]
fn oversize_file_degrades() {
    let (_project, root) = staged_project(&[
        ("big.rs", &oversize_source(11 * 1024 * 1024)),
        ("ok.rs", "pub fn fine() {}\n"),
    ]);
    let (_stage, mut host, mut child) = spawn_over(&root);

    assert_degraded(
        &mut host,
        &root.join("src/big.rs"),
        "file_too_large",
        "LMWV-RUST-FILE-TOO-LARGE",
    );

    host.shutdown().expect("shutdown must succeed");
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "plugin must exit 0; got {status:?}");
}

/// The init-time symbol-table walk must skip bombs: spawning over the bomb
/// project succeeds (asserted inside `spawn_over` — pre-guard this aborted at
/// `initialize`), and the skipped bomb contributes NOTHING to resolution — a
/// `use` of the bomb module resolves as if it did not exist, while a `use` of
/// the benign sibling still resolves to an `imports` edge (the non-vacuity
/// control).
#[test]
fn symbol_table_skips_bombs() {
    let (_project, root) = staged_project(&[
        ("bomb.rs", &deep_paren_bomb(3000)),
        ("ok.rs", "pub fn fine() {}\n"),
        (
            "user.rs",
            "use crate::bomb;\nuse crate::ok::fine;\n\npub fn call_it() {}\n",
        ),
    ]);
    let (_stage, mut host, mut child) = spawn_over(&root);

    let outcome = host
        .analyze_file(&root.join("src/user.rs"))
        .expect("user.rs must analyze");
    let imports: Vec<(&str, &str)> = outcome
        .edges
        .iter()
        .filter(|e| e.raw.kind == "imports")
        .map(|e| (e.raw.from_id.as_str(), e.raw.to_id.as_str()))
        .collect();
    // Control: the benign target resolved, so the resolver path is live.
    assert!(
        imports
            .iter()
            .any(|(_, to)| *to == "rust:function:bombproj.ok.fine"),
        "expected an imports edge to the benign fn; got {imports:?}"
    );
    // The bomb contributed nothing to the symbol table: no edge targets it.
    assert!(
        imports
            .iter()
            .all(|(_, to)| !to.contains("bomb.") && !to.ends_with("bombproj.bomb")),
        "the skipped bomb must resolve to nothing; got {imports:?}"
    );

    host.shutdown().expect("shutdown must succeed");
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "plugin must exit 0; got {status:?}");
}
