use rapidhash::{HashMapExt, RapidHashMap};
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
    states: RapidHashMap<K, CallsiteState>,
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
            states: RapidHashMap::new(),
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
