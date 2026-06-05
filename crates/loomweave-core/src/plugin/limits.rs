//! Core-enforced resource limits for the plugin host.
//!
//! Implements ADR-021 §2a–§2d: ceilings and circuit-breakers that plugins
//! cannot opt out of. All four minimums are defined here:
//!
//! | ADR ref   | Minimum                  | Type                    |
//! |-----------|--------------------------|-------------------------|
//! | ADR-021 §2a | Path-escape breaker    | [`PathEscapeBreaker`]   |
//! | ADR-021 §2b | Content-Length ceiling | [`ContentLengthCeiling`]|
//! | ADR-021 §2c | Entity/edge/finding cap| [`EntityCountCap`]      |
//! | ADR-021 §2d | Virtual-address limit  | [`apply_prlimit_as`]    |
//!
//! # Deferred: LMWV-INFRA-PLUGIN-ENTITY-OVERRUN-WARNING
//!
//! ADR-021 §2c also calls for a *warning* finding emitted when the cap is
//! approached (e.g. at 80 % of `DEFAULT_MAX`). This warning finding
//! (`LMWV-INFRA-PLUGIN-ENTITY-OVERRUN-WARNING`) is **not implemented in
//! Sprint 1**. It requires the Filigree scan-result ingest path that lands in
//! WP5/WP6; deferring avoids a hard dependency on that infrastructure.
//! When the ingest path is ready, add a `try_admit_with_warning` variant that
//! emits the warning finding before returning `Ok`.
//!
//! # Finding subcode constants
//!
//! Task 6 (the plugin supervisor) imports these constants to emit findings into
//! Filigree. The constants are defined here because they describe limit-domain
//! violations; the transport and jail modules do not emit findings themselves.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use thiserror::Error;

// ── Finding subcode constants (ADR-021, consumed by Task 6) ──────────────────

/// Finding subcode emitted when a plugin returns a path that escapes the jail.
pub const FINDING_PATH_ESCAPE: &str = "LMWV-INFRA-PLUGIN-PATH-ESCAPE";

/// Finding subcode emitted when the path-escape breaker trips and the plugin
/// is killed (the "disabled" sense: further entities from this plugin are
/// refused entirely).
pub const FINDING_DISABLED_PATH_ESCAPE: &str = "LMWV-INFRA-PLUGIN-DISABLED-PATH-ESCAPE";

/// Finding subcode emitted when `read_frame` rejects a frame because its
/// `Content-Length` exceeds the configured [`ContentLengthCeiling`].
pub const FINDING_FRAME_OVERSIZE: &str = "LMWV-INFRA-PLUGIN-FRAME-OVERSIZE";

/// Finding subcode emitted when [`EntityCountCap::try_admit`] returns
/// [`CapExceeded`] and the supervisor stops processing plugin output.
pub const FINDING_ENTITY_CAP: &str = "LMWV-INFRA-PLUGIN-ENTITY-CAP";

/// Finding subcode emitted when the plugin process is killed by the OS due to
/// exceeding its `RLIMIT_AS` memory ceiling (OOM kill on `RLIMIT_AS`).
pub const FINDING_OOM_KILLED: &str = "LMWV-INFRA-PLUGIN-OOM-KILLED";

// ── ContentLengthCeiling (ADR-021 §2b) ───────────────────────────────────────

/// Maximum permitted `Content-Length` value on an incoming plugin frame.
///
/// ADR-021 §2b sets the default at **8 MiB**. The operator may supply a lower
/// value via configuration; the 1 MiB config-surface floor mentioned in
/// ADR-021 is a deployment concern (not enforced in `new()`) — Sprint 1
/// hardcodes the default and does not expose configuration.
///
/// `Copy` — this is a single `usize`; pass by value throughout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentLengthCeiling(usize);

impl ContentLengthCeiling {
    /// Default ceiling: 8 MiB per ADR-021 §2b.
    pub const DEFAULT: Self = Self(8 * 1024 * 1024);

    /// Construct a ceiling with an explicit byte limit.
    ///
    /// The 1 MiB minimum floor from ADR-021 is a *configuration-surface*
    /// constraint, not enforced here — callers that construct a ceiling below
    /// 1 MiB are doing so deliberately (e.g. tight test budgets).
    pub const fn new(bytes: usize) -> Self {
        Self(bytes)
    }

    /// A sentinel ceiling that never fires — equivalent to `usize::MAX`.
    ///
    /// Use in tests that do not care about the frame-size limit.
    ///
    /// Gated behind `#[cfg(test)]` (production code would use this to
    /// `vec![0u8; isize::MAX]` on a malicious Content-Length and OOM-kill
    /// the host). Production callers must pass an explicit ceiling; the
    /// ADR-021 default is [`Self::DEFAULT`] at 8 MiB.
    #[cfg(test)]
    pub const fn unbounded() -> Self {
        Self(usize::MAX)
    }

    /// Return the ceiling value in bytes.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl Default for ContentLengthCeiling {
    fn default() -> Self {
        Self::DEFAULT
    }
}

// ── EntityCountCap error (ADR-021 §2c) ───────────────────────────────────────

/// Error returned by [`EntityCountCap::try_admit`] when the run-wide limit
/// would be exceeded.
///
/// ADR-021 §2c: entities, edges, and findings are all counted against the same
/// cap; the `delta` passed to `try_admit` is the combined increment.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("entity cap exceeded: {would_reach} items would be reached (cap {cap})")]
pub struct CapExceeded {
    /// The cumulative count that would have been reached.
    pub would_reach: usize,
    /// The configured cap.
    pub cap: usize,
}

// ── EntityCountCap (ADR-021 §2c) ─────────────────────────────────────────────

/// Cumulative counter guarding the run-wide entity + edge + finding cap.
///
/// ADR-021 §2c: the default cap is **500,000 items** (entities + edges +
/// findings combined). The supervisor calls [`try_admit`](Self::try_admit)
/// for each batch of items admitted from a plugin response; when the cap would
/// be exceeded the supervisor emits [`FINDING_ENTITY_CAP`] and stops
/// processing further plugin output.
///
/// `&mut self` — the cap lives inside the host's per-run state; Sprint 1 has
/// no cross-thread sharing of this value.
pub struct EntityCountCap {
    max: usize,
    consumed: usize,
}

impl EntityCountCap {
    /// Default cap: 500,000 items per ADR-021 §2c.
    pub const DEFAULT_MAX: usize = 500_000;

    /// Construct a cap with the given maximum.
    pub fn new(max: usize) -> Self {
        Self { max, consumed: 0 }
    }

    /// Attempt to admit `delta` more items.
    ///
    /// Returns `Ok(())` if `consumed + delta <= max`; otherwise returns
    /// [`CapExceeded`] and leaves `consumed` unchanged.
    ///
    /// The boundary case (`consumed + delta == max`) is admitted — the cap
    /// is reached but not exceeded.
    pub fn try_admit(&mut self, delta: usize) -> Result<(), CapExceeded> {
        let next = self.consumed.saturating_add(delta);
        if next > self.max {
            return Err(CapExceeded {
                would_reach: next,
                cap: self.max,
            });
        }
        self.consumed = next;
        Ok(())
    }

    /// Current cumulative count of admitted items.
    pub fn consumed(&self) -> usize {
        self.consumed
    }
}

// ── PathEscapeBreaker (ADR-021 §2a) ──────────────────────────────────────────

/// State returned by [`PathEscapeBreaker::record_escape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BreakerState {
    /// The breaker is within the escape threshold; the supervisor should drop
    /// the offending entity but keep the plugin alive.
    Open,
    /// The threshold has been exceeded; the supervisor should kill the plugin
    /// and emit [`FINDING_DISABLED_PATH_ESCAPE`].
    Tripped,
}

/// Rolling-window escape counter per ADR-021 §2a.
///
/// Trips when **more than 10** path escapes occur within a 60-second window.
/// The `>10` threshold (not `>=10`) is specified by ADR-021 §2a.
///
/// # Clock injection
///
/// The public API uses [`Instant::now()`] internally. Tests (and Task 6's
/// host code) use the crate-internal `record_escape_at` helper to inject
/// arbitrary instants without sleeping.
pub struct PathEscapeBreaker {
    /// Rolling window length — default 60 s per ADR-021 §2a.
    window: Duration,
    /// Breaker trips when `events.len() > threshold` after pruning.
    threshold: usize,
    /// Timestamps of recent path-escape events within the window.
    events: VecDeque<Instant>,
}

impl PathEscapeBreaker {
    /// Default window: 60 seconds per ADR-021 §2a.
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);
    /// Default threshold: 10 per ADR-021 §2a (trips on the **11th** escape).
    pub const DEFAULT_THRESHOLD: usize = 10;

    /// Construct a breaker with explicit window and threshold.
    pub fn new(window: Duration, threshold: usize) -> Self {
        Self {
            window,
            threshold,
            events: VecDeque::new(),
        }
    }

    /// Construct a breaker with the ADR-021 §2a defaults.
    pub fn new_default() -> Self {
        Self::new(Self::DEFAULT_WINDOW, Self::DEFAULT_THRESHOLD)
    }

    /// Record a path-escape event at `Instant::now()` and return the new
    /// breaker state.
    pub fn record_escape(&mut self) -> BreakerState {
        self.record_escape_at(Instant::now())
    }

    /// Test hook — accepts an injected `Instant` to make rolling-window pruning
    /// deterministic under test. Also used by Task 6's host code (same crate)
    /// when a precise timestamp is available. Not part of the public API.
    pub(crate) fn record_escape_at(&mut self, at: Instant) -> BreakerState {
        self.events.push_back(at);
        // Prune events outside the rolling window relative to `at`.
        self.events.retain(|&t| {
            // Keep events where `at - t < window`, i.e., `t > at - window`.
            // `at.checked_duration_since(t)` is `None` if `t > at` (future instant,
            // possible with injected clocks) — treat those as "just happened" (keep).
            at.checked_duration_since(t)
                .is_none_or(|age| age < self.window)
        });

        if self.events.len() > self.threshold {
            BreakerState::Tripped
        } else {
            BreakerState::Open
        }
    }
}

// ── apply_prlimit_as (ADR-021 §2d) ───────────────────────────────────────────

/// Default virtual-address space ceiling per ADR-021 §2d: **2 GiB**.
///
/// Applied via `RLIMIT_AS` in the plugin's child process before `exec`.
/// Task 6 calls `apply_prlimit_as` inside `CommandExt::pre_exec`.
pub const DEFAULT_MAX_RSS_MIB: u64 = 2 * 1024; // 2 GiB

/// Compute the effective RSS ceiling as the minimum of the manifest value and
/// the core default.
///
/// ADR-021 §2d: effective limit = `min(manifest.capabilities.runtime.expected_max_rss_mb, core_default)`.
/// A manifest value of 0 is treated as "unset" and the core default wins.
pub fn effective_rss_mib(manifest_value: u64, core_default: u64) -> u64 {
    if manifest_value == 0 {
        return core_default;
    }
    manifest_value.min(core_default)
}

/// Default file-descriptor ceiling applied to the plugin child. Plugin
/// work is ingest-style (open, parse, close — one file at a time); the
/// host's own minimum is much smaller than the shell's usual 1024/65535
/// `RLIMIT_NOFILE`. 256 covers any realistic concurrent-open pattern a
/// Sprint 1 plugin needs while preventing fd-flood `DoS` of the host's
/// own operations (logging, writer-actor `SQLite`).
pub const DEFAULT_MAX_NOFILE: u64 = 256;

/// Default process/thread ceiling applied to the plugin child. Sprint 1
/// plugins are single-threaded or run a tight tokio pool; 32 is an
/// order-of-magnitude ceiling that accommodates legitimate thread-pool
/// use while stopping fork-bomb / thread-flood `DoS`. Plugins that need
/// higher can negotiate via manifest extension in a later sprint; for
/// now the fixed default covers every planned v0.1 consumer.
pub const DEFAULT_MAX_NPROC: u64 = 32;

/// Apply `RLIMIT_AS` to the current process.
///
/// Called inside `CommandExt::pre_exec` (Task 6) so the limit applies to the
/// plugin child process, not the Loomweave host process. Setting the limit in
/// `pre_exec` is safe because `pre_exec` runs after `fork()` but before
/// `exec()`, so only the child's address-space limit is affected.
///
/// # Errors
///
/// Returns `std::io::Error` on `setrlimit` failure.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn apply_prlimit_as(max_rss_mib: u64) -> std::io::Result<()> {
    use nix::sys::resource::{Resource, setrlimit};

    let bytes = max_rss_mib.saturating_mul(1024 * 1024);
    setrlimit(Resource::RLIMIT_AS, bytes, bytes).map_err(std::io::Error::from)
}

/// Apply `RLIMIT_NOFILE` and `RLIMIT_NPROC` to the current process.
///
/// Called from the same `pre_exec` closure as [`apply_prlimit_as`]. Same
/// async-signal-safety notes apply — `setrlimit` is on the POSIX AS-safe
/// list, no allocation occurs, no Rust Drop runs. Failure from either
/// setrlimit returns `Err` and the stdlib `pre_exec` machinery aborts
/// the child via `_exit` before `exec`.
///
/// # Errors
///
/// Returns `std::io::Error` on the first `setrlimit` failure.
#[cfg(target_os = "linux")]
pub fn apply_prlimit_nofile_nproc(max_nofile: u64, max_nproc: u64) -> std::io::Result<()> {
    use nix::sys::resource::{Resource, setrlimit};

    setrlimit(Resource::RLIMIT_NOFILE, max_nofile, max_nofile).map_err(std::io::Error::from)?;
    setrlimit(Resource::RLIMIT_NPROC, max_nproc, max_nproc).map_err(std::io::Error::from)
}

/// Non-Linux stub for [`apply_prlimit_nofile_nproc`].
///
/// `nix` 0.28 does not expose `Resource::RLIMIT_NPROC` on macOS, so the real
/// implementation stays restricted to Linux.
#[cfg(not(target_os = "linux"))]
pub fn apply_prlimit_nofile_nproc(_max_nofile: u64, _max_nproc: u64) -> std::io::Result<()> {
    Ok(())
}

/// No-op stub for non-Linux/macOS targets (UQ-WP2-06: Linux-only for Sprint 1).
///
/// Logs a one-time warning and returns `Ok(())`. The caller proceeds without a
/// memory ceiling on the plugin process.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn apply_prlimit_as(_max_rss_mib: u64) -> std::io::Result<()> {
    warn_once_non_linux();
    Ok(())
}

/// Emit a one-time warning on non-Linux/macOS platforms.
///
/// Uses `std::sync::Once` rather than `tracing` — loomweave-core has no tracing
/// dep and we do not add one for this single warning (per task spec).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn warn_once_non_linux() {
    use std::sync::Once;
    static WARN: Once = Once::new();
    WARN.call_once(|| {
        eprintln!(
            "loomweave: RLIMIT_AS enforcement is Linux/macOS only; \
             plugin memory ceiling will not be applied on this platform"
        );
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ContentLengthCeiling tests ────────────────────────────────────────────

    /// DEFAULT equals 8 MiB.
    #[test]
    fn ceiling_default_is_8_mib() {
        assert_eq!(
            ContentLengthCeiling::DEFAULT.get(),
            8 * 1024 * 1024,
            "DEFAULT must be 8 MiB per ADR-021 §2b"
        );
    }

    /// `new` and `get` round-trip.
    #[test]
    fn ceiling_new_get_round_trip() {
        let c = ContentLengthCeiling::new(12345);
        assert_eq!(c.get(), 12345);
    }

    /// `Default` impl returns the same as `DEFAULT`.
    #[test]
    fn ceiling_default_impl_matches_constant() {
        assert_eq!(
            ContentLengthCeiling::default().get(),
            ContentLengthCeiling::DEFAULT.get()
        );
    }

    /// `unbounded()` returns `usize::MAX`.
    #[test]
    fn ceiling_unbounded_is_usize_max() {
        assert_eq!(ContentLengthCeiling::unbounded().get(), usize::MAX);
    }

    // ── EntityCountCap tests ──────────────────────────────────────────────────

    /// Admit under the cap → Ok.
    #[test]
    fn cap_admit_under_limit_returns_ok() {
        let mut cap = EntityCountCap::new(100);
        assert!(cap.try_admit(50).is_ok());
        assert_eq!(cap.consumed(), 50);
    }

    /// Admit to the exact boundary → Ok (boundary is inclusive).
    #[test]
    fn cap_admit_at_exact_boundary_returns_ok() {
        let mut cap = EntityCountCap::new(100);
        assert!(cap.try_admit(100).is_ok());
        assert_eq!(cap.consumed(), 100);
    }

    /// Admit one over the boundary → `CapExceeded`.
    #[test]
    fn cap_admit_over_boundary_returns_cap_exceeded() {
        let mut cap = EntityCountCap::new(100);
        let err = cap.try_admit(101).expect_err("should exceed cap");
        assert_eq!(err.cap, 100);
        assert_eq!(err.would_reach, 101);
        // consumed must be unchanged after a failed admit.
        assert_eq!(cap.consumed(), 0, "failed admit must not modify consumed");
    }

    /// Cumulative admits: multiple calls accumulate correctly.
    #[test]
    fn cap_cumulative_admits_accumulate() {
        let mut cap = EntityCountCap::new(500_000);
        for _ in 0..9 {
            cap.try_admit(50_000).expect("under cap");
        }
        assert_eq!(cap.consumed(), 450_000);
        // One more batch of 50k hits exact boundary.
        cap.try_admit(50_000).expect("at exact boundary");
        assert_eq!(cap.consumed(), 500_000);
        // One more item tips over.
        let err = cap.try_admit(1).expect_err("must exceed");
        assert_eq!(err.cap, 500_000);
    }

    // ── PathEscapeBreaker tests ───────────────────────────────────────────────

    /// 10 escapes → Open (threshold is >10, not >=10).
    #[test]
    fn breaker_ten_escapes_returns_open() {
        let mut b = PathEscapeBreaker::new_default();
        let t = Instant::now();
        let mut state = BreakerState::Open;
        for _ in 0..10 {
            state = b.record_escape_at(t);
        }
        assert_eq!(
            state,
            BreakerState::Open,
            "10 escapes must not trip the breaker (threshold is >10)"
        );
    }

    /// 11th escape → Tripped.
    #[test]
    fn breaker_eleventh_escape_returns_tripped() {
        let mut b = PathEscapeBreaker::new_default();
        let t = Instant::now();
        for _ in 0..10 {
            b.record_escape_at(t);
        }
        let state = b.record_escape_at(t);
        assert_eq!(
            state,
            BreakerState::Tripped,
            "11th escape must trip the breaker"
        );
    }

    /// Events older than the window are pruned; only recent events count.
    ///
    /// Push 10 events at t0, then 1 event at t0+61s → Open (10 old events
    /// pruned, only 1 within-window). Then one more → still Open (2 in window).
    #[test]
    fn breaker_old_events_pruned_outside_window() {
        let mut b = PathEscapeBreaker::new_default();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(61); // outside 60s window

        for _ in 0..10 {
            b.record_escape_at(t0);
        }
        // 10 events at t0. Now record at t1 — t0-events are >60s old from t1.
        let state = b.record_escape_at(t1);
        assert_eq!(
            state,
            BreakerState::Open,
            "after pruning 10 old events, 1 within-window event must leave breaker Open"
        );

        // One more at t1 → 2 within-window events → still Open.
        let state2 = b.record_escape_at(t1);
        assert_eq!(
            state2,
            BreakerState::Open,
            "2 events in window must be Open"
        );
    }

    // ── effective_rss_mib tests ───────────────────────────────────────────────

    /// Manifest value smaller than core default → manifest wins.
    #[test]
    fn effective_rss_mib_manifest_wins_when_smaller() {
        assert_eq!(effective_rss_mib(256, 2048), 256);
    }

    /// Manifest value larger than core default → core default wins.
    #[test]
    fn effective_rss_mib_core_ceiling_wins_when_larger() {
        assert_eq!(effective_rss_mib(4096, 2048), 2048);
    }

    /// Manifest value of 0 → treated as unset, core default used.
    #[test]
    fn effective_rss_mib_zero_manifest_uses_core_default() {
        assert_eq!(effective_rss_mib(0, 2048), 2048);
    }

    // ── apply_prlimit_as tests ────────────────────────────────────────────────

    /// On Linux: calling `apply_prlimit_as` with the default ceiling returns Ok.
    ///
    /// `setrlimit` lowers the current process hard limit, so the actual syscall
    /// probe runs in a child test process. That keeps default parallel test runs
    /// from starving sibling tests after this probe succeeds.
    #[cfg(target_os = "linux")]
    #[test]
    fn apply_prlimit_linux_returns_ok() {
        let output = std::process::Command::new(std::env::current_exe().expect("current exe"))
            .args([
                "--exact",
                "plugin::limits::tests::apply_prlimit_linux_child_process_returns_ok",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .expect("spawn child prlimit test");
        assert!(
            output.status.success(),
            "child prlimit test failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "invoked by apply_prlimit_linux_returns_ok in a child process"]
    fn apply_prlimit_linux_child_process_returns_ok() {
        let result = apply_prlimit_as(DEFAULT_MAX_RSS_MIB);
        assert!(result.is_ok(), "apply_prlimit_as must succeed: {result:?}");
    }

    #[test]
    fn nofile_nproc_limit_is_not_enabled_for_macos() {
        let source = include_str!("limits.rs");
        assert!(
            source.contains(
                "#[cfg(target_os = \"linux\")]\n\
pub fn apply_prlimit_nofile_nproc"
            ),
            "RLIMIT_NOFILE/RLIMIT_NPROC helper must stay Linux-only; \
             nix 0.28 does not expose Resource::RLIMIT_NPROC on macOS"
        );
        assert!(
            !source.contains(
                "#[cfg(any(target_os = \"linux\", target_os = \"macos\"))]\n\
pub fn apply_prlimit_nofile_nproc"
            ),
            "macOS must not compile the RLIMIT_NPROC branch"
        );
    }

    /// On non-Linux: the stub path compiles and returns Ok (type-level check).
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn apply_prlimit_non_linux_stub_returns_ok() {
        let result = apply_prlimit_nofile_nproc(DEFAULT_MAX_NOFILE, DEFAULT_MAX_NPROC);
        assert!(result.is_ok());
    }
}
