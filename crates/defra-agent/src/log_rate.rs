//! Process-wide log-rate ceiling.
//!
//! Guardrail for the incident class in #588: a single hot code path emitted
//! unthrottled `WARN` events at ~350k lines/sec, saturating journald, CPU,
//! and IO until the host wedged. No individual callsite should be able to
//! flood the host journal, whatever its cause.
//!
//! [`CallsiteRateLimiter`] is the pure, deterministic core: a per-callsite
//! fixed-window counter that admits up to `max_per_window` events per window
//! and reports how many were suppressed when a new window opens.
//! [`RateLimitFilter`] adapts it as a `tracing_subscriber` per-layer filter:
//! suppression is never silent — when the next event from a suppressed
//! callsite is admitted in a later window, a summary event reports the
//! suppressed count. A callsite that goes permanently quiet after a
//! suppressed stretch never flushes its final tail count; that is deliberate
//! — its first `max_per_window` events of the window were already logged,
//! and a quiet callsite is no longer a threat to the host.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing_subscriber::layer::{Context, Filter};

/// Target used for suppression summary events. Exempt from rate limiting so
/// the summaries themselves can never recurse into suppression.
pub const SUMMARY_TARGET: &str = "defra_agent::log_rate";

/// Configuration for the per-callsite log-rate ceiling.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    /// Maximum events admitted per callsite within one window.
    pub max_per_window: u32,
    /// Length of the fixed window.
    pub window: Duration,
}

impl Default for RateLimitConfig {
    /// 100 events per second per callsite: far above any legitimate logging
    /// pattern, three orders of magnitude below the #588 flood.
    fn default() -> Self {
        Self {
            max_per_window: 100,
            window: Duration::from_secs(1),
        }
    }
}

/// Outcome of admitting one event through the limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Admit the event.
    Allow,
    /// Admit the event, and report that `suppressed` events were dropped at
    /// this callsite since the last admitted one.
    AllowWithSummary { suppressed: u64 },
    /// Drop the event.
    Suppress,
}

/// Deterministic fixed-window rate limiter keyed by callsite.
#[derive(Debug)]
pub struct CallsiteRateLimiter<K> {
    config: RateLimitConfig,
    states: HashMap<K, CallsiteState>,
}

#[derive(Debug)]
struct CallsiteState {
    window_start: Instant,
    admitted_in_window: u32,
    suppressed: u64,
}

impl<K: Eq + Hash> CallsiteRateLimiter<K> {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            states: HashMap::new(),
        }
    }

    /// Records one event at `key` observed at `now` and decides its fate.
    pub fn check(&mut self, key: K, now: Instant) -> Decision {
        let state = self.states.entry(key).or_insert(CallsiteState {
            window_start: now,
            admitted_in_window: 0,
            suppressed: 0,
        });

        if now.saturating_duration_since(state.window_start) >= self.config.window {
            let suppressed = state.suppressed;
            state.window_start = now;
            state.admitted_in_window = 1;
            state.suppressed = 0;
            return if suppressed > 0 {
                Decision::AllowWithSummary { suppressed }
            } else {
                Decision::Allow
            };
        }

        if state.admitted_in_window < self.config.max_per_window {
            state.admitted_in_window += 1;
            Decision::Allow
        } else {
            state.suppressed += 1;
            Decision::Suppress
        }
    }
}

/// `tracing_subscriber` per-layer filter enforcing the ceiling on events.
///
/// Spans always pass; only events are rate-limited. When a suppressed
/// stretch ends, a `WARN` summary event on [`SUMMARY_TARGET`] reports the
/// suppressed count and the affected target, so suppression is observable.
///
/// Summaries cannot be emitted inline: `tracing` drops events dispatched
/// while another dispatch is in progress on the same thread (the dispatcher
/// thread-local is re-entrancy guarded), so a summary emitted from within
/// `Filter::enabled` would vanish. A dedicated summariser thread emits them
/// through the process-global dispatcher instead. Consequence: summaries are
/// only visible on the global subscriber — which is how every production
/// binary initializes telemetry — not on thread-scoped test subscribers.
pub struct RateLimitFilter {
    limiter: Mutex<CallsiteRateLimiter<tracing::callsite::Identifier>>,
    now: fn() -> Instant,
    summaries: std::sync::mpsc::Sender<SuppressionSummary>,
}

struct SuppressionSummary {
    suppressed: u64,
    original_target: String,
}

impl RateLimitFilter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self::with_clock(config, Instant::now)
    }

    /// Test seam: inject a controllable clock.
    pub fn with_clock(config: RateLimitConfig, now: fn() -> Instant) -> Self {
        let (summaries, rx) = std::sync::mpsc::channel::<SuppressionSummary>();
        std::thread::Builder::new()
            .name("log-rate-summaries".into())
            .spawn(move || {
                // Exits when the owning filter (the sender) is dropped. This
                // thread never has a scoped dispatcher, so the event goes to
                // the global default.
                for summary in rx {
                    tracing::warn!(
                        target: SUMMARY_TARGET,
                        suppressed = summary.suppressed,
                        original_target = %summary.original_target,
                        "rate limiter suppressed repeated log events from one callsite"
                    );
                }
            })
            .expect("spawning log-rate summariser thread");
        Self {
            limiter: Mutex::new(CallsiteRateLimiter::new(config)),
            now,
            summaries,
        }
    }

    fn check(&self, meta: &tracing::Metadata<'_>) -> Decision {
        self.limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .check(meta.callsite(), (self.now)())
    }
}

impl<S: tracing::Subscriber> Filter<S> for RateLimitFilter {
    fn enabled(&self, meta: &tracing::Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        // Spans always pass: only event volume floods the journal, and span
        // metadata is what makes the admitted events diagnosable.
        if !meta.is_event() || meta.target() == SUMMARY_TARGET {
            return true;
        }

        match self.check(meta) {
            Decision::Allow => true,
            Decision::AllowWithSummary { suppressed } => {
                // A dead summariser thread only costs the summary, never the
                // event.
                let _ = self.summaries.send(SuppressionSummary {
                    suppressed,
                    original_target: meta.target().to_string(),
                });
                true
            }
            Decision::Suppress => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_per_window: u32, window_secs: u64) -> RateLimitConfig {
        RateLimitConfig {
            max_per_window,
            window: Duration::from_secs(window_secs),
        }
    }

    #[test]
    fn admits_events_under_the_window_limit() {
        let mut limiter = CallsiteRateLimiter::new(config(3, 10));
        let start = Instant::now();

        for i in 0..3 {
            assert_eq!(
                limiter.check("site-a", start + Duration::from_millis(i)),
                Decision::Allow,
                "event {i} should be admitted"
            );
        }
    }

    #[test]
    fn suppresses_events_over_the_window_limit() {
        let mut limiter = CallsiteRateLimiter::new(config(2, 10));
        let start = Instant::now();

        assert_eq!(limiter.check("site-a", start), Decision::Allow);
        assert_eq!(limiter.check("site-a", start), Decision::Allow);
        assert_eq!(limiter.check("site-a", start), Decision::Suppress);
        assert_eq!(limiter.check("site-a", start), Decision::Suppress);
    }

    #[test]
    fn reports_suppressed_count_when_a_new_window_opens() {
        let mut limiter = CallsiteRateLimiter::new(config(1, 10));
        let start = Instant::now();

        assert_eq!(limiter.check("site-a", start), Decision::Allow);
        assert_eq!(limiter.check("site-a", start), Decision::Suppress);
        assert_eq!(limiter.check("site-a", start), Decision::Suppress);

        assert_eq!(
            limiter.check("site-a", start + Duration::from_secs(11)),
            Decision::AllowWithSummary { suppressed: 2 }
        );
    }

    #[test]
    fn rollover_without_suppression_is_a_plain_allow() {
        let mut limiter = CallsiteRateLimiter::new(config(2, 10));
        let start = Instant::now();

        assert_eq!(limiter.check("site-a", start), Decision::Allow);
        assert_eq!(
            limiter.check("site-a", start + Duration::from_secs(11)),
            Decision::Allow
        );
    }

    #[test]
    fn suppressed_count_resets_after_being_reported() {
        let mut limiter = CallsiteRateLimiter::new(config(1, 10));
        let start = Instant::now();

        assert_eq!(limiter.check("site-a", start), Decision::Allow);
        assert_eq!(limiter.check("site-a", start), Decision::Suppress);
        assert_eq!(
            limiter.check("site-a", start + Duration::from_secs(11)),
            Decision::AllowWithSummary { suppressed: 1 }
        );

        // Next rollover with no interim suppression reports nothing.
        assert_eq!(
            limiter.check("site-a", start + Duration::from_secs(22)),
            Decision::Allow
        );
    }

    #[test]
    fn callsites_are_limited_independently() {
        let mut limiter = CallsiteRateLimiter::new(config(1, 10));
        let start = Instant::now();

        assert_eq!(limiter.check("site-a", start), Decision::Allow);
        assert_eq!(limiter.check("site-a", start), Decision::Suppress);
        assert_eq!(limiter.check("site-b", start), Decision::Allow);
        assert_eq!(limiter.check("site-b", start), Decision::Suppress);
        assert_eq!(limiter.check("site-a", start), Decision::Suppress);
    }

    #[test]
    fn flood_within_one_window_admits_exactly_the_limit() {
        let mut limiter = CallsiteRateLimiter::new(config(100, 1));
        let start = Instant::now();

        let admitted = (0..350_000)
            .filter(|_| limiter.check("hot-loop", start) != Decision::Suppress)
            .count();

        assert_eq!(admitted, 100);
    }

    mod filter {
        use std::io;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Arc, Mutex, OnceLock};

        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::Layer;

        use super::*;

        /// Controllable clock for [`RateLimitFilter::with_clock`]: a fixed
        /// base instant plus a test-adjustable millisecond offset.
        static CLOCK_BASE: OnceLock<Instant> = OnceLock::new();
        static CLOCK_OFFSET_MS: AtomicU64 = AtomicU64::new(0);

        fn test_clock() -> Instant {
            *CLOCK_BASE.get_or_init(Instant::now)
                + Duration::from_millis(CLOCK_OFFSET_MS.load(Ordering::SeqCst))
        }

        #[derive(Clone, Default)]
        struct CapturedOutput(Arc<Mutex<Vec<u8>>>);

        impl CapturedOutput {
            fn lines(&self) -> Vec<String> {
                String::from_utf8(self.0.lock().expect("capture lock").clone())
                    .expect("utf8 log output")
                    .lines()
                    .map(str::to_string)
                    .collect()
            }
        }

        impl io::Write for CapturedOutput {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("capture lock").extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for CapturedOutput {
            type Writer = CapturedOutput;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        // The flood-cap + suppression-summary end-to-end test lives in
        // `tests/log_rate_filter.rs`: summaries are emitted through the
        // process-global dispatcher, which a `--lib` test cannot install
        // without leaking into every other test in the binary.

        #[test]
        fn filter_passes_spans_through_untouched() {
            let output = CapturedOutput::default();
            let filter = RateLimitFilter::with_clock(
                RateLimitConfig {
                    max_per_window: 1,
                    window: Duration::from_secs(60),
                },
                test_clock,
            );
            let subscriber = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::layer()
                    .with_writer(output.clone())
                    .with_ansi(false)
                    .with_filter(filter),
            );

            tracing::subscriber::with_default(subscriber, || {
                for i in 0..10 {
                    let span = tracing::info_span!("loop_span", i);
                    let _guard = span.enter();
                    tracing::info!(i, "span-scoped event");
                }
            });

            let lines = output.lines();
            let event_lines = lines
                .iter()
                .filter(|l| l.contains("span-scoped event"))
                .count();
            assert_eq!(event_lines, 1, "events are rate limited even inside spans");
        }
    }
}
