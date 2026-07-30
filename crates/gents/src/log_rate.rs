//! Process-wide log-rate ceiling.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing_subscriber::layer::{Context, Filter};

pub const SUMMARY_TARGET: &str = "gents::log_rate";

#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    pub max_per_window: u32,
    pub window: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_per_window: 100,
            window: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    AllowWithSummary { suppressed: u64 },
    Suppress,
}

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

    pub fn with_clock(config: RateLimitConfig, now: fn() -> Instant) -> Self {
        let (summaries, rx) = std::sync::mpsc::channel::<SuppressionSummary>();
        std::thread::Builder::new()
            .name("log-rate-summaries".into())
            .spawn(move || {
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
        if !meta.is_event() || meta.target() == SUMMARY_TARGET {
            return true;
        }

        match self.check(meta) {
            Decision::Allow => true,
            Decision::AllowWithSummary { suppressed } => {
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
