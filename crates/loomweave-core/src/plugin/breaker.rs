//! Crash-loop breaker for plugin spawn attempts.
//!
//! Implements ADR-002 (crash-loop breaker) and UQ-WP2-10 (threshold = >3
//! crashes in 60 s). When the breaker trips, the host refuses further
//! spawn attempts for the rolling-window duration.
//!
//! Sprint 1 hard-codes the threshold and window per UQ-WP2-10; the config
//! surface (`loomweave.yaml:plugin_limits.crash_*`) lands in WP6.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

// ── Finding subcode constants ─────────────────────────────────────────────────

/// Subcode emitted when the breaker trips.
pub const FINDING_DISABLED_CRASH_LOOP: &str = "LMWV-INFRA-PLUGIN-DISABLED-CRASH-LOOP";

// ── CrashLoopState ────────────────────────────────────────────────────────────

/// State returned by [`CrashLoopBreaker::record_crash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CrashLoopState {
    /// Within the threshold; further spawn attempts allowed.
    Open,
    /// Threshold exceeded; further spawn attempts should be refused.
    Tripped,
}

// ── CrashLoopBreaker ──────────────────────────────────────────────────────────

/// Rolling-window crash counter.
///
/// Trips when **more than 3** plugin crashes occur within a 60-second window
/// per ADR-002 + UQ-WP2-10. The `>3` threshold (not `>=3`) is specified by
/// UQ-WP2-10.
///
/// # Clock injection
///
/// The public API uses [`Instant::now()`] internally. Tests use the
/// crate-internal `record_crash_at` helper to inject arbitrary instants
/// without sleeping.
pub struct CrashLoopBreaker {
    /// Rolling window length — default 60 s per ADR-002 + UQ-WP2-10.
    window: Duration,
    /// Breaker trips when `events.len() > threshold` after pruning.
    threshold: usize,
    /// Timestamps of recent crash events within the window.
    events: VecDeque<Instant>,
}

impl CrashLoopBreaker {
    /// 60 s rolling window per ADR-002 + UQ-WP2-10.
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);
    /// >3 crashes per window trips per UQ-WP2-10.
    pub const DEFAULT_THRESHOLD: usize = 3;

    /// Construct a breaker with explicit window and threshold.
    pub fn new(window: Duration, threshold: usize) -> Self {
        Self {
            window,
            threshold,
            events: VecDeque::new(),
        }
    }

    /// Record a crash at `Instant::now()` and return the resulting state.
    pub fn record_crash(&mut self) -> CrashLoopState {
        self.record_crash_at(Instant::now())
    }

    /// Current state without recording a new crash (useful pre-spawn check).
    ///
    /// Counts events within the rolling window and compares to the threshold.
    /// Does NOT eagerly prune stale events from the underlying storage; call
    /// [`record_crash`](Self::record_crash) (or `record_crash_at`) to trigger
    /// pruning. In practice the `VecDeque` length is bounded by crash rate ×
    /// window, so unbounded-growth-without-record_crash is not a realistic
    /// production scenario.
    pub fn state(&self) -> CrashLoopState {
        let now = Instant::now();
        let live_count = self
            .events
            .iter()
            .filter(|&&t| {
                now.checked_duration_since(t)
                    .is_none_or(|age| age < self.window)
            })
            .count();
        if live_count > self.threshold {
            CrashLoopState::Tripped
        } else {
            CrashLoopState::Open
        }
    }

    /// Test hook — accepts an injected `Instant` to make rolling-window
    /// pruning deterministic under test. Crate-internal.
    pub(crate) fn record_crash_at(&mut self, at: Instant) -> CrashLoopState {
        self.events.push_back(at);
        // Prune events outside the rolling window relative to `at`.
        self.events.retain(|&t| {
            // Keep events where `at - t < window`, i.e., `t > at - window`.
            // `at.checked_duration_since(t)` is `None` if `t > at` (future
            // instant, possible with injected clocks) — treat as "just
            // happened" (keep).
            at.checked_duration_since(t)
                .is_none_or(|age| age < self.window)
        });

        if self.events.len() > self.threshold {
            CrashLoopState::Tripped
        } else {
            CrashLoopState::Open
        }
    }
}

impl Default for CrashLoopBreaker {
    fn default() -> Self {
        Self::new(Self::DEFAULT_WINDOW, Self::DEFAULT_THRESHOLD)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests (pure breaker, no real sleep) ──────────────────────────────

    /// `breaker_01`: new breaker returns Open from `state()`.
    #[test]
    fn breaker_01_zero_crashes_is_open() {
        let b = CrashLoopBreaker::default();
        assert_eq!(
            b.state(),
            CrashLoopState::Open,
            "fresh breaker must be Open"
        );
    }

    /// `breaker_02`: record 3 crashes at a single Instant; `state()` returns Open.
    /// Threshold is >3, so exactly 3 crashes does not trip it.
    #[test]
    fn breaker_02_at_threshold_stays_open() {
        let mut b = CrashLoopBreaker::default();
        let t = Instant::now();
        let mut state = CrashLoopState::Open;
        for _ in 0..3 {
            state = b.record_crash_at(t);
        }
        assert_eq!(
            state,
            CrashLoopState::Open,
            "3 crashes must not trip the breaker (threshold is >3 per UQ-WP2-10)"
        );
    }

    /// `breaker_03`: 4th crash trips the breaker.
    #[test]
    fn breaker_03_above_threshold_trips() {
        let mut b = CrashLoopBreaker::default();
        let t = Instant::now();
        for _ in 0..3 {
            b.record_crash_at(t);
        }
        let state = b.record_crash_at(t);
        assert_eq!(
            state,
            CrashLoopState::Tripped,
            "4th crash must trip the breaker"
        );
    }

    /// `breaker_04`: 4 crashes at t0; advance 61 s and record 1 more → Open
    /// (old crashes pruned out of the window).
    #[test]
    fn breaker_04_old_crashes_pruned() {
        let mut b = CrashLoopBreaker::default();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(61); // outside the 60 s window

        for _ in 0..4 {
            b.record_crash_at(t0);
        }
        // Breaker is tripped at t0.
        // Now record one crash at t1 — t0 events age out; only this new one remains.
        let state = b.record_crash_at(t1);
        assert_eq!(
            state,
            CrashLoopState::Open,
            "after pruning 4 old events, 1 within-window crash must leave breaker Open"
        );
    }

    /// `breaker_05`: `CrashLoopBreaker::default()` uses documented constants.
    #[test]
    fn breaker_05_default_values() {
        assert_eq!(
            CrashLoopBreaker::DEFAULT_WINDOW,
            Duration::from_secs(60),
            "DEFAULT_WINDOW must be 60 s per ADR-002 + UQ-WP2-10"
        );
        assert_eq!(
            CrashLoopBreaker::DEFAULT_THRESHOLD,
            3,
            "DEFAULT_THRESHOLD must be 3 per UQ-WP2-10"
        );
        // Also verify the Default impl delegates to new().
        let b = CrashLoopBreaker::default();
        assert_eq!(b.window, CrashLoopBreaker::DEFAULT_WINDOW);
        assert_eq!(b.threshold, CrashLoopBreaker::DEFAULT_THRESHOLD);
    }

    // ── Integration test with MockPlugin::new_crashing ────────────────────────

    /// `breaker_06`: simulate the production crash-loop pattern using
    /// `MockPlugin::new_crashing`.
    ///
    /// The crashing mock transitions to Crashed after the `initialized`
    /// notification; all further frames are silently dropped. We drive the
    /// mock at the transport layer (`write_frame` / `read_frame` / tick) rather
    /// than through `PluginHost`, because `PluginHost`'s private fields are not
    /// accessible from this module.
    ///
    /// Each cycle:
    ///   1. Build a fresh `MockPlugin::new_crashing()`.
    ///   2. Send `initialize` + tick → read back the initialize response.
    ///   3. Send `initialized` notification + tick → mock transitions to Crashed.
    ///   4. Send `analyze_file` + tick → mock silently drops it; the outbox
    ///      has grown by zero bytes since the initialized notification.
    ///      This zero-byte response is the "crash" signal.
    ///   5. Treat the absence of a response as a crash: `record_crash()`.
    ///
    /// Assert: after the 4th cycle the breaker is Tripped; the 5th cycle
    /// pre-checks `state()` and skips the mock drive entirely.
    #[test]
    fn breaker_06_mock_crash_loop_trips_breaker() {
        use crate::plugin::limits::ContentLengthCeiling;
        use crate::plugin::mock::MockPlugin;
        use crate::plugin::protocol::{
            AnalyzeFileParams, InitializeParams, InitializedNotification, make_notification,
            make_request,
        };
        use crate::plugin::transport::{Frame, read_frame, write_frame};

        let mut breaker = CrashLoopBreaker::default();
        let mut crashes_recorded: usize = 0;

        for cycle in 1..=5_usize {
            // Pre-spawn state check: once tripped, refuse further attempts.
            if breaker.state() == CrashLoopState::Tripped {
                assert!(
                    crashes_recorded >= 4,
                    "cycle {cycle}: breaker tripped before 4 recorded crashes (got {crashes_recorded})"
                );
                continue;
            }

            let mut mock = MockPlugin::new_crashing();

            // Step 1: send initialize request.
            let init_req = make_request(
                "initialize",
                &InitializeParams {
                    protocol_version: "1.0".to_owned(),
                    project_root: "/tmp/loomweave-breaker-test".to_owned(),
                },
                1,
            );
            write_frame(
                mock.stdin(),
                &Frame {
                    body: serde_json::to_vec(&init_req).expect("serialise initialize"),
                },
            )
            .expect("write initialize");
            mock.tick().expect("tick after initialize");

            // Step 2: read initialize response — crashing mock responds normally here.
            let _init_resp_frame =
                read_frame(mock.stdout(), ContentLengthCeiling::new(1024 * 1024)).unwrap_or_else(
                    |e| panic!("cycle {cycle}: crashing mock must respond to initialize: {e}"),
                );

            // Step 3: send initialized notification → mock transitions to Crashed.
            let init_note = make_notification("initialized", &InitializedNotification {});
            write_frame(
                mock.stdin(),
                &Frame {
                    body: serde_json::to_vec(&init_note).expect("serialise initialized"),
                },
            )
            .expect("write initialized");
            mock.tick().expect("tick after initialized");

            // Record outbox position after the handshake.
            let pos_after_handshake = mock.stdout().get_ref().len() as u64;

            // Step 4: send analyze_file → Crashed mock silently drops it.
            let af_req = make_request(
                "analyze_file",
                &AnalyzeFileParams {
                    file_path: "/tmp/loomweave-breaker-test/stub.mock".to_owned(),
                },
                2,
            );
            write_frame(
                mock.stdin(),
                &Frame {
                    body: serde_json::to_vec(&af_req).expect("serialise analyze_file"),
                },
            )
            .expect("write analyze_file");
            mock.tick()
                .expect("tick after analyze_file (crashing mock)");

            // The outbox must not have grown — crashed mock produces no response.
            let pos_after_analyze = mock.stdout().get_ref().len() as u64;
            assert_eq!(
                pos_after_analyze, pos_after_handshake,
                "cycle {cycle}: crashing mock must produce no response to analyze_file \
                 (outbox grew from {pos_after_handshake} to {pos_after_analyze})"
            );

            // The absence of an analyze_file response is the "crash" signal.
            // In production the host's read_frame would block / return EOF;
            // here we treat the zero outbox growth as equivalent.
            let state = breaker.record_crash();
            crashes_recorded += 1;

            if cycle == 3 {
                assert_eq!(
                    state,
                    CrashLoopState::Open,
                    "cycle {cycle}: 3 crashes must not trip the breaker (threshold is >3)"
                );
            }
            if cycle == 4 {
                assert_eq!(
                    state,
                    CrashLoopState::Tripped,
                    "cycle {cycle}: 4th crash must trip the breaker"
                );
            }
        }

        assert_eq!(
            crashes_recorded, 4,
            "must have recorded exactly 4 crashes before breaker refused the 5th cycle"
        );
        assert_eq!(
            breaker.state(),
            CrashLoopState::Tripped,
            "breaker must be Tripped after 4 crashes"
        );
    }
}
