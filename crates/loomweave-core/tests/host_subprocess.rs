//! T1 — subprocess happy path integration test.
//!
//! Spawns the `loomweave-plugin-fixture` binary via [`PluginHost::spawn`],
//! performs the full handshake, issues one `analyze_file` for a fixture file,
//! receives one entity, shuts down cleanly, and asserts exit code 0.
//!
//! The fixture binary is located at runtime by searching the Cargo target
//! directory. This is necessary because `CARGO_BIN_EXE_*` is only available
//! for binaries in the same crate; cross-crate binary resolution requires
//! either `-Z bindeps` (unstable) or a runtime search.

use loomweave_core::PluginHost;
use loomweave_core::plugin::parse_manifest;

/// Path to the fixture plugin.toml — embedded at compile time.
const FIXTURE_MANIFEST_BYTES: &[u8] = include_bytes!("fixtures/plugin.toml");

/// Locate the `loomweave-fixture-plugin` binary in the Cargo target directory.
///
/// The cargo artifact is named `loomweave-fixture-plugin` (off the
/// `loomweave-plugin-*` discovery glob — see the fixture crate's Cargo.toml).
/// Tests that spawn it stage it under its `loomweave-plugin-fixture` manifest
/// name first (see [`staged_fixture`]).
///
/// Searches the standard Cargo output locations in order:
/// 1. `CARGO_BIN_EXE_loomweave-fixture-plugin` env var (set by cargo nextest
///    when artifact deps are enabled — future use).
/// 2. `<target_dir>/debug/loomweave-fixture-plugin` (default dev build).
/// 3. `<target_dir>/release/loomweave-fixture-plugin` (release build).
///
/// Builds the fixture on demand and panics with a clear message if the binary
/// still cannot be found.
fn fixture_binary_path() -> std::path::PathBuf {
    // Check if an explicit path was provided (e.g. by a future artifact dep).
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_loomweave-fixture-plugin") {
        return std::path::PathBuf::from(path);
    }

    // Locate the workspace target directory via CARGO_MANIFEST_DIR.
    // CARGO_MANIFEST_DIR for an integration test is the crate's directory.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // loomweave-core is at crates/loomweave-core; workspace root is ../../
    let workspace_root = manifest_dir
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("workspace root must exist");

    // Try CARGO_TARGET_DIR override first, then the default `target/` directory.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map_or_else(|_| workspace_root.join("target"), std::path::PathBuf::from);

    if let Some(path) = find_fixture_binary(&target_dir) {
        return path;
    }

    build_fixture_binary(workspace_root, &target_dir);

    if let Some(path) = find_fixture_binary(&target_dir) {
        return path;
    }

    panic!(
        "loomweave-fixture-plugin binary not found. \
         Tried `cargo build -p loomweave-plugin-fixture --bin loomweave-fixture-plugin`. \
         Searched in: {}",
        target_dir.display()
    );
}

/// Stage the fixture binary under its manifest-declared basename
/// (`loomweave-plugin-fixture`) and return `(tempdir, staged_path)`.
///
/// The cargo artifact is named `loomweave-fixture-plugin` (off the
/// `loomweave-plugin-*` discovery glob), so it cannot be spawned directly:
/// [`PluginHost::spawn`] requires the binary's basename to equal the manifest's
/// `plugin.executable` (`loomweave-plugin-fixture`). Staging a copy under that
/// name mirrors how a real install presents the binary. Keep the returned
/// `TempDir` alive for the duration of the spawn.
fn staged_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("create fixture staging dir");
    let staged = dir.path().join(format!(
        "loomweave-plugin-fixture{}",
        std::env::consts::EXE_SUFFIX
    ));
    // `copy` preserves the exec bit on Unix and compiles on all platforms
    // (unlike `os::unix::fs::symlink`); this test is not `cfg(unix)`-gated.
    std::fs::copy(fixture_binary_path(), &staged).expect("stage fixture binary");
    (dir, staged)
}

fn find_fixture_binary(target_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for profile in &["debug", "release"] {
        let candidate = target_dir.join(profile).join(format!(
            "loomweave-fixture-plugin{}",
            std::env::consts::EXE_SUFFIX
        ));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn build_fixture_binary(workspace_root: &std::path::Path, target_dir: &std::path::Path) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = std::process::Command::new(cargo)
        .current_dir(workspace_root)
        .arg("build")
        .arg("-p")
        .arg("loomweave-plugin-fixture")
        .arg("--bin")
        .arg("loomweave-fixture-plugin")
        .arg("--target-dir")
        .arg(target_dir)
        .output()
        .expect("spawn cargo build for loomweave-plugin-fixture");

    assert!(
        output.status.success(),
        "cargo build for loomweave-plugin-fixture failed with status {}.\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Verify the fixture manifest parses correctly.
/// This catches schema mismatches before the subprocess test runs.
#[test]
fn fixture_manifest_parses_correctly() {
    let manifest = parse_manifest(FIXTURE_MANIFEST_BYTES).expect("fixture manifest must parse");
    assert_eq!(manifest.plugin.plugin_id, "fixture");
    assert_eq!(manifest.ontology.entity_kinds, vec!["widget"]);
    assert_eq!(manifest.ontology.rule_id_prefix, "LMWV-FIXTURE-");
    assert!(
        !manifest.capabilities.runtime.reads_outside_project_root,
        "fixture manifest must not request reads_outside_project_root"
    );
}

/// T1: subprocess happy path.
///
/// Spawns the fixture plugin, completes the handshake, analyzes a real file,
/// receives one entity, shuts down, and asserts exit code 0.
#[test]
fn t1_subprocess_happy_path() {
    // 1. Parse the fixture manifest. Leave `plugin.executable` as declared
    //    in the TOML (a bare basename); spawn validates it matches the
    //    discovered binary's basename.
    let manifest =
        parse_manifest(FIXTURE_MANIFEST_BYTES).expect("fixture plugin.toml must be valid");

    // 2. Build a real project root containing the fixture sample file.
    let project_dir = tempfile::TempDir::new().expect("create tempdir");
    let sample_path = project_dir.path().join("sample.mt");
    std::fs::write(&sample_path, b"widget demo.sample {}\n").expect("write sample.mt");

    // 3. Spawn the plugin with the discovered binary path, staged under its
    //    manifest-declared `loomweave-plugin-fixture` basename so spawn's
    //    basename check passes.
    let (_fixture_stage, exec) = staged_fixture();
    let (mut host, mut child) =
        PluginHost::spawn(manifest, project_dir.path(), &exec).expect("spawn must succeed");

    // 5. Analyze the fixture file.
    let outcome = host
        .analyze_file(&sample_path)
        .expect("analyze_file must succeed");

    // 6. Assert: exactly one entity, zero edges (fixture plugin emits no edges).
    assert_eq!(
        outcome.entities.len(),
        1,
        "fixture plugin must return exactly one entity per analyze_file; got {}",
        outcome.entities.len()
    );
    assert!(
        outcome.edges.is_empty(),
        "fixture plugin must return no edges; got {}",
        outcome.edges.len()
    );
    let entity = &outcome.entities[0];
    assert_eq!(
        entity.kind, "widget",
        "entity kind must be 'widget'; got {:?}",
        entity.kind
    );
    assert_eq!(
        entity.id.as_str(),
        "fixture:widget:demo.sample",
        "entity id must be 'fixture:widget:demo.sample'; got {:?}",
        entity.id.as_str()
    );

    // 7. Shut down cleanly.
    host.shutdown().expect("shutdown must succeed");

    // 8. Wait for the child and assert exit code 0.
    let status = child.wait().expect("wait for child process");
    assert!(
        status.success(),
        "fixture plugin must exit with code 0; got: {status:?}"
    );

    // 9. No unexpected findings.
    let findings = host.take_findings();
    assert!(
        findings.is_empty(),
        "no findings expected on happy path; got: {findings:?}"
    );
}

/// T9: handshake failure on a subprocess that exits before responding
/// returns `Err` promptly — the host does not hang on a closed stdout.
///
/// Points `executable` at `/bin/true` (or Windows equivalent), which exits
/// immediately. The host tries to read the initialize response from a closed
/// stdout and returns a transport error.
///
/// **What this test asserts**: `spawn()` returns `Err` and the whole call
/// completes well under 5 s. Zombie-reap coverage lives in the Linux-only
/// `/proc` test below.
///
/// The earlier name `t9_handshake_failure_exits_cleanly_without_hanging`
/// overstated this — "exits cleanly" implied zombie-reap coverage.
#[test]
#[cfg(unix)]
fn t9_handshake_failure_on_immediate_exit_returns_err_promptly() {
    let manifest = parse_manifest(FIXTURE_MANIFEST_BYTES).expect("fixture manifest must parse");

    let project_dir = tempfile::TempDir::new().expect("tmpdir");

    // Construct a symlink whose basename matches the manifest-declared
    // `plugin.executable` (`loomweave-plugin-fixture`) but whose target is
    // `/bin/true`. This exits immediately without reading stdin, which is
    // the handshake-failure mode we want to test. Pointing `spawn` at
    // `/bin/true` directly would fail the basename-match check before
    // forking, which tests a different property.
    let stub_dir = tempfile::TempDir::new().expect("stub dir");
    let stub_exec = stub_dir.path().join("loomweave-plugin-fixture");
    std::os::unix::fs::symlink("/bin/true", &stub_exec).expect("symlink /bin/true");

    let start = std::time::Instant::now();
    let result = PluginHost::spawn(manifest, project_dir.path(), &stub_exec);
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "spawn must fail when executable exits before handshake response"
    );
    // Sanity: the handshake-failure path must not block. If reap lost a
    // waitpid, this would still return but a regression that swapped kill()
    // or wait() for a blocking read on the closed pipe would hang here.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "handshake failure must return promptly; took {elapsed:?}"
    );
}

/// T9a: handshake failure reaps the subprocess.
///
/// The stub records its PID then exits without speaking JSON-RPC. If
/// `PluginHost::spawn` drops the `Child` without `wait()`, Linux keeps that PID
/// as a zombie owned by this test process. The assertion below must fail in
/// that regression.
#[test]
#[cfg(target_os = "linux")]
fn t9a_handshake_failure_reaps_exited_subprocess() {
    let manifest = parse_manifest(FIXTURE_MANIFEST_BYTES).expect("fixture manifest must parse");

    let project_dir = tempfile::TempDir::new().expect("tmpdir");
    let stub_dir = tempfile::TempDir::new().expect("stub dir");
    let pid_file = stub_dir.path().join("plugin.pid");
    let stub_exec = stub_dir.path().join("loomweave-plugin-fixture");
    std::fs::write(
        &stub_exec,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > {}\nexit 0\n",
            shell_quote(&pid_file)
        ),
    )
    .expect("write handshake-failing stub");
    let mut perms = std::fs::metadata(&stub_exec)
        .expect("stub metadata")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&stub_exec, perms).expect("chmod stub");

    let result = PluginHost::spawn(manifest, project_dir.path(), &stub_exec);

    assert!(
        result.is_err(),
        "spawn must fail when executable exits before handshake response"
    );
    let pid = read_recorded_pid(&pid_file);
    assert_not_linux_zombie(pid);
}

#[cfg(target_os = "linux")]
fn read_recorded_pid(path: &std::path::Path) -> u32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            return contents
                .trim()
                .parse::<u32>()
                .expect("stub must record numeric pid");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "stub did not record its pid at {}",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn assert_not_linux_zombie(pid: u32) {
    let status_path = std::path::PathBuf::from(format!("/proc/{pid}/status"));
    let Ok(status) = std::fs::read_to_string(&status_path) else {
        return;
    };
    let state = status
        .lines()
        .find(|line| line.starts_with("State:"))
        .unwrap_or("State: <missing>");
    assert!(
        !state.contains("zombie") && !state.contains("\tZ"),
        "handshake-failed subprocess pid {pid} was not reaped: {state}"
    );
}

#[cfg(target_os = "linux")]
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// T9b: `stderr_tail()` is wired on subprocess-backed hosts. The fixture
/// plugin does not write to stderr on the happy path, so the tail is
/// `Some("")` or `Some(<small>)`; the key assertion is that it's `Some`
/// (not `None`) — the drain thread is attached and reachable. `None`
/// after spawn would indicate the stderr ring was never installed.
#[test]
#[cfg(unix)]
fn t9b_stderr_tail_is_some_after_spawn() {
    let manifest = parse_manifest(FIXTURE_MANIFEST_BYTES).expect("fixture manifest must parse");
    let project_dir = tempfile::TempDir::new().expect("tmpdir");
    let sample_path = project_dir.path().join("sample.mt");
    std::fs::write(&sample_path, b"widget demo.sample {}\n").expect("write sample.mt");

    let (_fixture_stage, exec) = staged_fixture();
    let (mut host, mut child) =
        PluginHost::spawn(manifest, project_dir.path(), &exec).expect("spawn must succeed");

    // The tail must be Some — drain thread is wired. Content may vary
    // (the fixture doesn't write to stderr on success paths, so empty
    // is expected).
    let tail = host.stderr_tail();
    assert!(
        tail.is_some(),
        "subprocess host must expose Some(stderr_tail); got None"
    );

    host.shutdown().expect("shutdown");
    let _ = child.wait();
}

/// T10: `PluginHost::spawn` refuses a manifest whose `plugin.executable`
/// contains a path separator. A compromised `plugin.toml` must not be
/// able to redirect execution to `/bin/sh`, `python3`, or a relative
/// traversal; the manifest field is required to be a bare basename
/// matching the PATH-discovered binary.
#[test]
#[cfg(unix)]
fn t10_manifest_executable_with_path_separator_is_refused() {
    use loomweave_core::HostError;

    let mut manifest = parse_manifest(FIXTURE_MANIFEST_BYTES).expect("fixture manifest must parse");
    manifest.plugin.executable = "/bin/sh".to_owned();

    let project_dir = tempfile::TempDir::new().expect("tmpdir");
    let exec = fixture_binary_path();

    let Err(err) = PluginHost::spawn(manifest, project_dir.path(), &exec) else {
        panic!("spawn must refuse absolute-path manifest executable");
    };
    match err {
        HostError::Spawn(msg) => {
            assert!(
                msg.contains("path separator"),
                "spawn error must name the path-separator violation; got: {msg}"
            );
        }
        other => panic!("expected HostError::Spawn; got {other:?}"),
    }
}

/// T11: `PluginHost::spawn` refuses a manifest whose `plugin.executable`
/// basename does not match the PATH-discovered binary. Prevents a plugin
/// directory hosting two binaries from accidentally cross-wiring: the
/// host never runs a binary with a different name than the manifest
/// declares.
#[test]
#[cfg(unix)]
fn t11_manifest_executable_basename_mismatch_is_refused() {
    use loomweave_core::HostError;

    let mut manifest = parse_manifest(FIXTURE_MANIFEST_BYTES).expect("fixture manifest must parse");
    // Declare a basename that will not match the discovered binary.
    manifest.plugin.executable = "loomweave-plugin-other".to_owned();

    let project_dir = tempfile::TempDir::new().expect("tmpdir");
    let exec = fixture_binary_path();

    let Err(err) = PluginHost::spawn(manifest, project_dir.path(), &exec) else {
        panic!("spawn must refuse basename mismatch");
    };
    match err {
        HostError::Spawn(msg) => {
            assert!(
                msg.contains("does not match") && msg.contains("basename"),
                "spawn error must name the basename mismatch; got: {msg}"
            );
        }
        other => panic!("expected HostError::Spawn; got {other:?}"),
    }
}
