//! End-to-end test for the log-rate ceiling (#588): a flooding callsite is
//! capped and the suppressed count is reported via a summary event.
//!
//! This lives in its own integration binary because suppression summaries
//! are emitted through the process-global dispatcher (see
//! `defra_agent::log_rate`), so the test must own `set_global_default` —
//! which cannot be done inside the shared `--lib` test binary.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use defra_agent::log_rate::{RateLimitConfig, RateLimitFilter, SUMMARY_TARGET};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

/// Controllable clock: a fixed base instant plus a test-adjustable offset.
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

#[test]
fn filter_caps_a_flood_and_reports_suppression_on_rollover() {
    let output = CapturedOutput::default();
    let filter = RateLimitFilter::with_clock(
        RateLimitConfig {
            max_per_window: 5,
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
    tracing::subscriber::set_global_default(subscriber).expect("sole global subscriber");

    // 1000 events in one window, then one more from the same callsite in a
    // fresh window: that first admitted event triggers the suppression
    // summary (emitted asynchronously from the summariser thread).
    for i in 0..=1_000 {
        if i == 1_000 {
            CLOCK_OFFSET_MS.fetch_add(61_000, Ordering::SeqCst);
        }
        tracing::warn!(i, "hot loop event");
    }

    let lines = output.lines();
    let hot_lines = lines
        .iter()
        .filter(|l| l.contains("hot loop event"))
        .count();
    assert_eq!(
        hot_lines, 6,
        "the window limit plus the one post-rollover event"
    );
    assert!(
        lines.iter().any(|l| l.contains("i=1000")),
        "post-rollover event should be admitted"
    );

    // The summary arrives asynchronously; wait for it, bounded.
    let deadline = Instant::now() + Duration::from_secs(10);
    let summary_seen = loop {
        if output
            .lines()
            .iter()
            .any(|l| l.contains(SUMMARY_TARGET) && l.contains("suppressed=995"))
        {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        summary_seen,
        "suppression summary missing or wrong: {:#?}",
        output.lines()
    );
}
