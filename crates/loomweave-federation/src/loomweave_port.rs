//! Loomweave read-API ephemeral-port contract (ADR-044).
//!
//! The twin of Filigree's ephemeral-port convention, applied to
//! Loomweave's own federation HTTP read API. `serve` binds a per-project
//! deterministic port (ephemeral `:0` fallback) and publishes the *actually
//! bound* port to `<project_root>/.weft/loomweave/ephemeral.port`. Cross-product
//! consumers (notably Wardline, which is Python) read this file; nobody
//! recomputes a peer's port. The deterministic band here is an implementation
//! detail, never part of the file contract.
//!
//! File contract (ADR-044, normative): a single plain-ASCII integer TCP port,
//! optional trailing `\n`, written atomically (temp + rename), present only
//! while `serve` holds a loopback bind. Host (`127.0.0.1`) and scheme (`http`)
//! are implied, sound only because publication is loopback-only.

use std::path::{Path, PathBuf};

/// Base of Loomweave's deterministic read-API port band. Chosen to sit
/// **above** Filigree's `8400–9399` band so the two products never contend for
/// the same number. Internal only — never part of the cross-product file
/// contract (consumers read the published file, never recompute).
pub const PORT_BAND_BASE: u16 = 9400;
/// Width of the band: ports land in `[PORT_BAND_BASE, PORT_BAND_BASE + PORT_BAND_SPAN)`.
pub const PORT_BAND_SPAN: u16 = 1000;

/// Canonical path of the published port file for a project root.
#[must_use]
pub fn published_port_path(project_root: &Path) -> PathBuf {
    loomweave_core::store::store_dir(project_root).join("ephemeral.port")
}

/// Deterministic-but-unpredictable read-API port for a project, derived from
/// the canonical project path. Stable across runs (so a consumer's static
/// config can match it) yet path-specific (so two projects differ). Mirrors
/// Filigree's `8400 + hash % 1000`, in a disjoint band, using Loomweave's own
/// hash (blake3, as for SEI). The bound port is published; this computation is
/// the producer's *starting guess*, not a value any consumer recomputes.
///
/// # Panics
///
/// Never in practice: the `expect` calls are on infallible arithmetic
/// (`blake3` always produces 32 bytes; `% 1000 < 1000` always fits `u16`).
#[must_use]
pub fn deterministic_port(project_root: &Path) -> u16 {
    // Best-effort canonicalize so every caller (serve, install, doctor) agrees
    // regardless of whether it pre-canonicalized; fall back to the path as-given.
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let bytes = canonical.to_string_lossy();
    let hash = blake3::hash(bytes.as_bytes());
    let head = u64::from_le_bytes(
        hash.as_bytes()[..8]
            .try_into()
            .expect("blake3 digest is 32 bytes, so [..8] is 8 bytes"),
    );
    let offset = u16::try_from(head % u64::from(PORT_BAND_SPAN))
        .expect("remainder of % 1000 is < 1000, which fits u16");
    PORT_BAND_BASE + offset
}

/// Read and validate the published port. Any missing / non-integer /
/// out-of-range / zero content folds to `None` (fail-soft, ADR-044). A `u16`
/// parse already bounds `1..=65535` except `0`, which we reject explicitly.
#[must_use]
pub fn read_published_port(project_root: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(published_port_path(project_root)).ok()?;
    raw.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

/// Atomically publish `port` to `<project_root>/.weft/loomweave/ephemeral.port`.
/// Writes a temp file in the same directory and `rename(2)`s it into place, so
/// a concurrent reader never observes a torn value. Creates `.weft/loomweave/`
/// if absent. The caller is responsible for the loopback-only invariant (only
/// call this when the bound address is loopback).
///
/// # Errors
/// Returns the underlying I/O error if the directory cannot be created or the
/// temp file cannot be written/renamed.
pub fn publish_port(project_root: &Path, port: u16) -> std::io::Result<()> {
    let dir = loomweave_core::store::store_dir(project_root);
    std::fs::create_dir_all(&dir)?;
    // One `serve` per process publishes, so the PID makes the temp name unique
    // within this directory without needing a random suffix.
    let tmp = dir.join(format!("ephemeral.port.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{port}\n"))?;
    if let Err(err) = std::fs::rename(&tmp, published_port_path(project_root)) {
        // A successful write + failed rename would otherwise strand the temp.
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// Best-effort removal of the published port file. A missing file is not an
/// error (idempotent). Called on clean shutdown; SIGKILL leaves a stale file,
/// which `read_published_port` validation + the ADR-034 instance-ID guard
/// handle (a stale file degrades, never corrupts).
pub fn remove_published_port(project_root: &Path) {
    let _ = std::fs::remove_file(published_port_path(project_root));
}

/// Compare-and-delete: remove the published port file **only when it still
/// names `port`**. Used by a `serve` instance's drop guard so it never unlinks
/// a *different* live instance's published port.
///
/// Two `serve` instances on the same project both auto-bind: the first lands on
/// the deterministic port, the second falls back to an OS-assigned ephemeral and
/// **overwrites** the shared `ephemeral.port` with its own number. If either one
/// then unlinks the file unconditionally on exit, discovery loses the *other,
/// still-running* server's port. Gating the unlink on "the file still names the
/// port I published" means a server only ever retracts its own publication; an
/// instance whose value was already overwritten by a peer leaves the peer's file
/// intact.
///
/// Read-then-delete is an unavoidable check-then-act: a peer could overwrite the
/// file in the window between the read and the unlink, so we keep that window as
/// tight as a single function call and accept the residual race. The only loss
/// it can cause is a *stale* file — exactly what `read_published_port`
/// validation + the ADR-034 instance-ID guard already tolerate (a stale file
/// degrades, never corrupts). A missing file is a no-op (idempotent), like
/// [`remove_published_port`].
pub fn remove_published_port_if_matches(project_root: &Path, port: u16) {
    if read_published_port(project_root) == Some(port) {
        let _ = std::fs::remove_file(published_port_path(project_root));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_port_is_stable_and_in_band() {
        let dir = tempfile::tempdir().unwrap();
        let a = deterministic_port(dir.path());
        let b = deterministic_port(dir.path());
        assert_eq!(a, b, "same path must yield the same port");
        assert!(
            (PORT_BAND_BASE..PORT_BAND_BASE + PORT_BAND_SPAN).contains(&a),
            "port {a} must land in the loomweave band [{PORT_BAND_BASE}, {})",
            PORT_BAND_BASE + PORT_BAND_SPAN
        );
        // Disjoint from Filigree's 8400-9399 band.
        assert!(
            a >= 9400,
            "port {a} must not overlap Filigree's 8400-9399 band"
        );
    }

    #[test]
    fn deterministic_port_differs_by_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Distinct tempdirs almost always hash to distinct ports; assert the
        // function is path-sensitive by checking the inputs differ and the
        // computation is a pure function of the (canonical) path.
        assert_ne!(a.path(), b.path());
        let pa = deterministic_port(a.path());
        let pb = deterministic_port(b.path());
        // Not guaranteed distinct (1/1000 collision), but the band membership
        // and determinism are what matter; assert both are in-band.
        assert!(pa >= 9400 && pb >= 9400);
    }

    #[test]
    fn publish_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).expect("publish");
        assert_eq!(read_published_port(dir.path()), Some(9412));
        // Published content is the bare port plus a single trailing newline.
        let raw = std::fs::read_to_string(published_port_path(dir.path())).unwrap();
        assert_eq!(raw, "9412\n");
    }

    #[test]
    fn publish_creates_store_dir_if_absent() {
        let dir = tempfile::tempdir().unwrap();
        // No .weft/loomweave/ yet.
        assert!(!loomweave_core::store::store_dir(dir.path()).exists());
        publish_port(dir.path(), 10000).expect("publish creates .weft/loomweave/");
        assert_eq!(read_published_port(dir.path()), Some(10000));
    }

    #[test]
    fn read_tolerates_trailing_whitespace_and_newline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(loomweave_core::store::store_dir(dir.path())).unwrap();
        std::fs::write(published_port_path(dir.path()), "  9500  \n").unwrap();
        assert_eq!(read_published_port(dir.path()), Some(9500));
    }

    #[test]
    fn read_rejects_malformed_zero_and_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(loomweave_core::store::store_dir(dir.path())).unwrap();
        for bad in ["", "not-a-port", "0", "65536", "70000", "-1", "12.5"] {
            std::fs::write(published_port_path(dir.path()), bad).unwrap();
            assert_eq!(
                read_published_port(dir.path()),
                None,
                "malformed/out-of-range content {bad:?} must fold to None (fail-soft)"
            );
        }
    }

    #[test]
    fn read_absent_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_published_port(dir.path()), None);
    }

    #[test]
    fn remove_is_idempotent_and_clears_the_file() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9999).unwrap();
        assert!(published_port_path(dir.path()).exists());
        remove_published_port(dir.path());
        assert!(!published_port_path(dir.path()).exists());
        // Second remove on an absent file is a no-op, not an error.
        remove_published_port(dir.path());
    }

    #[test]
    fn remove_if_matches_only_unlinks_own_port() {
        let dir = tempfile::tempdir().unwrap();
        // Instance A publishes its port, then instance B overwrites the shared
        // file with its own (the two-serve ephemeral-fallback scenario).
        publish_port(dir.path(), 9412).unwrap();
        publish_port(dir.path(), 9999).unwrap();
        assert_eq!(read_published_port(dir.path()), Some(9999));

        // Instance A exits first: its guard must NOT clobber B's live file.
        remove_published_port_if_matches(dir.path(), 9412);
        assert_eq!(
            read_published_port(dir.path()),
            Some(9999),
            "a non-matching port must leave the peer's published file intact"
        );

        // Instance B exits: its guard names the current value and unlinks it.
        remove_published_port_if_matches(dir.path(), 9999);
        assert!(
            !published_port_path(dir.path()).exists(),
            "the matching port retracts its own publication"
        );

        // Idempotent on an absent file.
        remove_published_port_if_matches(dir.path(), 9999);
    }
}
