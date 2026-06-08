//! Task 13 — host-integration roundtrip: handshake → `analyze_file` → shutdown.
//!
//! Spawns the real `loomweave-plugin-rust` subprocess through
//! [`PluginHost::spawn`] (which performs the full `initialize`/`initialized`
//! handshake internally — see `loomweave-core/tests/host_subprocess.rs`),
//! issues one `analyze_file` for a tiny Rust source file laid out as a real
//! crate, and asserts the expected `struct`/`function` ids round-trip back
//! through the host's ontology/identity/jail/cap validation. Then shuts the
//! subprocess down cleanly and asserts exit code 0.
//!
//! The cargo artifact is named `loomweave-rust-plugin` (OFF the
//! `loomweave-plugin-*` discovery glob — see this crate's Cargo.toml); it is
//! staged under its manifest-declared basename `loomweave-plugin-rust` before
//! spawning so `spawn`'s basename-match check passes, mirroring how a real
//! install presents the binary.

use loomweave_core::PluginHost;
use loomweave_core::plugin::parse_manifest;

/// This crate's shipped manifest — embedded at compile time.
const RUST_MANIFEST_BYTES: &[u8] = include_bytes!("../plugin.toml");

/// A tiny, analysable Rust source file. Laid out under a temp crate's `src/`
/// so crate-root discovery (ADR-049 §1) produces real crate-rooted ids.
const SAMPLE_RS: &str =
    "pub struct Gadget { pub n: i32 }\npub fn make() -> Gadget { Gadget { n: 0 } }\n";

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
    (dir, staged)
}

/// Lay out a one-crate project root with the sample under `src/sample.rs`, so
/// the plugin's init-time crate-root discovery + module-path resolution produce
/// crate-rooted ADR-049 ids. Returns `(project_dir, sample_path)`.
fn staged_sample_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let project_dir = tempfile::TempDir::new().expect("create project dir");
    let src = project_dir.path().join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    std::fs::write(
        project_dir.path().join("Cargo.toml"),
        "[package]\nname = \"sample_crate\"\n",
    )
    .expect("write Cargo.toml");
    // A lib.rs makes `src/` the recognised crate source root.
    std::fs::write(src.join("lib.rs"), "pub mod sample;\n").expect("write lib.rs");
    let sample_path = src.join("sample.rs");
    std::fs::write(&sample_path, SAMPLE_RS).expect("write sample.rs");
    (project_dir, sample_path)
}

/// Task 13: the full handshake → `analyze_file` → shutdown roundtrip through the
/// real host. The handshake (`initialize`/`initialized`) runs inside
/// `PluginHost::spawn`; `shutdown` drives the subprocess to exit 0.
#[test]
fn handshake_analyze_shutdown_roundtrip() {
    let manifest = parse_manifest(RUST_MANIFEST_BYTES).expect("rust plugin.toml must be valid");

    let (_project_stage, sample_path) = staged_sample_project();
    let project_root = sample_path
        .parent() // src/
        .and_then(|p| p.parent()) // project root
        .expect("project root")
        .to_path_buf();

    let (_binary_stage, exec) = staged_rust_plugin();
    let (mut host, mut child) =
        PluginHost::spawn(manifest, &project_root, &exec).expect("spawn must succeed");

    let outcome = host
        .analyze_file(&sample_path)
        .expect("analyze_file must succeed");

    let ids: Vec<String> = outcome
        .entities
        .iter()
        .map(|e| e.id.as_str().to_owned())
        .collect();

    assert!(
        ids.iter().any(|id| id.starts_with("rust:struct:")),
        "expected a rust:struct: entity; got {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.starts_with("rust:function:")),
        "expected a rust:function: entity; got {ids:?}"
    );
    // The crate-rooted module path must reach back to the sample's crate.
    assert!(
        ids.iter()
            .any(|id| id == "rust:struct:sample_crate.sample.Gadget"),
        "expected crate-rooted struct id; got {ids:?}"
    );
    assert!(
        ids.iter()
            .any(|id| id == "rust:function:sample_crate.sample.make"),
        "expected crate-rooted function id; got {ids:?}"
    );

    host.shutdown().expect("shutdown must succeed");

    let status = child.wait().expect("wait for child process");
    assert!(
        status.success(),
        "rust plugin must exit with code 0; got: {status:?}"
    );
}
