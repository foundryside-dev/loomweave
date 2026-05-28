//! `clarion hook session-start` — fail-soft session-start orientation.
//!
//! Never returns an error to the caller: the SessionStart hook must never
//! block an agent's session start. All failures degrade to a printed note.

use std::path::Path;

/// Run `clarion hook session-start`. Always returns `Ok(())`.
pub fn session_start(path: &Path) -> anyhow::Result<()> {
    println!("Clarion: orientation hook (snapshot wired in Task 3.2).");
    println!(
        "If briefings look empty, run `clarion analyze {}`.",
        path.display()
    );
    Ok(())
}
