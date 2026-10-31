use rapidhash::{HashMapExt, RapidHashMap};
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing_subscriber::layer::{Context, Filter};

pub const SUMMARY_TARGET: &str = "gents::observe::log_rate";

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

impl<K> CallsiteRateLimiter<K>
where
    K: Eq + Hash,
{
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            states: RapidHashMap::new(),
        }
    }

    pub fn check(&mut self, key: K, now: Instant) -> Decision {
        self.check_with_config(key, now, self.config)
    }

    fn check_with_config(&mut self, key: K, now: Instant, config: RateLimitConfig) -> Decision {
        let state = self.states.entry(key).or_insert(CallsiteState {
            window_start: now,
            admitted_in_window: 0,
            suppressed: 0,
        });

        if now.saturating_duration_since(state.window_start) >= config.window {
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

        if state.admitted_in_window < config.max_per_window {
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
    original_file: Option<String>,
    original_line: Option<u32>,
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
                        original_file = summary.original_file.as_deref(),
                        original_line = summary.original_line,
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
        let mut limiter = self
            .limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let config = if is_worker_poll_report(meta) {
            RateLimitConfig {
                max_per_window: 1,
                window: Duration::from_secs(60),
            }
        } else {
            limiter.config
        };
        limiter.check_with_config(meta.callsite(), (self.now)(), config)
    }
}

impl<S> Filter<S> for RateLimitFilter
where
    S: tracing::Subscriber,
{
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
                    original_file: meta.file().map(str::to_owned),
                    original_line: meta.line(),
                });
                true
            }
            Decision::Suppress => false,
        }
    }
}

/// The pinned workers owner reports slow polls at this target with these fields.
/// Its panic diagnostics share the target and must retain the ordinary ceiling.
fn is_worker_poll_report(meta: &tracing::Metadata<'_>) -> bool {
    meta.target() == "workers::worker"
        && *meta.level() == tracing::Level::WARN
        && ["task", "worker", "elapsed_ms", "limit_ms"]
            .iter()
            .all(|field| meta.fields().field(field).is_some())
}
