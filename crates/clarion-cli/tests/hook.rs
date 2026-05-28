//! `clarion hook session-start` integration tests.

use assert_cmd::Command;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn hook_session_start_exits_zero_without_clarion_db() {
    // Fail-soft: no .clarion/ at all must still exit 0 and nudge.
    let dir = tempfile::tempdir().unwrap();
    let assert = clarion_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        out.contains("clarion analyze"),
        "missing analyze nudge in: {out}"
    );
}
