//! Kill a plugin child together with everything it spawned.
//!
//! A language plugin is rarely a single process: the Python plugin runs a
//! `pyright-langserver` wrapper that in turn runs `node`. Killing only the
//! direct child leaves the grandchild behind — reparented to pid 1 and, when
//! it is wedged (the case that made the watchdog fire in the first place),
//! spinning forever (clarion-ebf404dfbb: a pyright at 103 % CPU for 1 h 40 m
//! with no analyze running). So every host kill path collects the child's
//! descendants FIRST (they are reparented the instant the child dies and
//! would no longer be findable through it), kills the child, then kills the
//! descendants.
//!
//! Descendants come from `/proc/<pid>/task/<tid>/children` (Linux; needs
//! `CONFIG_PROC_CHILDREN`, on by default in distribution kernels). Without
//! `/proc` (macOS) the walk yields nothing and only the direct child is
//! killed — the pre-existing behaviour. No process group is used: putting the
//! plugin in its own group would stop a terminal's Ctrl-C from reaching it
//! alongside `analyze`, orphaning it in exactly the interactive case that
//! today works.

use std::process::Child;

/// Every live descendant pid of `pid`, deepest last. Empty when the process
/// tree cannot be walked (non-Linux, or the process already exited).
#[must_use]
pub fn descendant_pids(pid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut frontier = vec![pid];
    while let Some(parent) = frontier.pop() {
        for child in direct_children(parent) {
            if !found.contains(&child) {
                found.push(child);
                frontier.push(child);
            }
        }
    }
    found
}

#[cfg(target_os = "linux")]
fn direct_children(pid: u32) -> Vec<u32> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for task in tasks.flatten() {
        let Ok(list) = std::fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        children.extend(
            list.split_whitespace()
                .filter_map(|p| p.parse::<u32>().ok()),
        );
    }
    children
}

#[cfg(not(target_os = "linux"))]
fn direct_children(_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// SIGKILL `child` and every process it spawned. The returned error is the
/// direct child's `kill` outcome (a child that already exited is not an
/// error); descendant kills are best-effort — a pid that vanished between
/// the walk and the signal is the desired end state.
///
/// # Errors
///
/// Propagates [`Child::kill`]'s error.
pub fn kill_process_tree(child: &mut Child) -> std::io::Result<()> {
    let descendants = descendant_pids(child.id());
    let result = child.kill();
    for pid in descendants {
        kill_pid(pid);
    }
    result
}

#[cfg(unix)]
fn kill_pid(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    if let Ok(raw) = i32::try_from(pid) {
        let _ = kill(Pid::from_raw(raw), Signal::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        // A zombie still has a /proc entry; require a non-zombie state.
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
    }

    #[test]
    fn kills_the_grandchild_the_direct_child_spawned() {
        // sh -> sleep: the shell is our child, sleep its grandchild. Neither
        // gets stdio so a pipe EOF cannot be what ends them.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 300 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchildren = loop {
            let found = descendant_pids(child.id());
            if !found.is_empty() || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            grandchildren.len(),
            1,
            "sleep must be visible as a descendant"
        );
        let sleeper = grandchildren[0];
        assert!(alive(sleeper));

        kill_process_tree(&mut child).expect("kill child");
        let _ = child.wait();

        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(sleeper) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !alive(sleeper),
            "grandchild {sleeper} must not outlive the tree kill"
        );
    }

    #[test]
    fn exited_process_has_no_descendants() {
        let mut child = Command::new("true").spawn().expect("spawn true");
        let _ = child.wait();
        assert!(descendant_pids(child.id()).is_empty());
    }
}
