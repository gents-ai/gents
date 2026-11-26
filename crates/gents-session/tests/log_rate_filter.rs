use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gents_session::log_rate::{RateLimitConfig, RateLimitFilter, SUMMARY_TARGET};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

static CLOCK_BASE: OnceLock<Instant> = OnceLock::new();
static CLOCK_OFFSET_MS: AtomicU64 = AtomicU64::new(0);

fn test_clock() -> Instant {
    *CLOCK_BASE.get_or_init(Instant::now)
        + Duration::from_millis(CLOCK_OFFSET_MS.load(Ordering::SeqCst))
}

#[derive(Clone, Default)]
struct CapturedOutput(Arc<CapturedBuffer>);

#[derive(Default)]
struct CapturedBuffer {
    bytes: Mutex<Vec<u8>>,
    changed: Condvar,
}

impl CapturedOutput {
    fn lines(&self) -> Vec<String> {
        String::from_utf8(self.0.bytes.lock().expect("capture lock").clone())
            .expect("utf8 log output")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn wait_for<F>(&self, timeout: Duration, condition: F) -> bool
    where
        F: Fn(&[u8]) -> bool,
    {
        let bytes = self.0.bytes.lock().expect("capture lock");
        let (bytes, _) = self
            .0
            .changed
            .wait_timeout_while(bytes, timeout, |bytes| !condition(bytes))
            .expect("capture notification");
        condition(&bytes)
    }
}

impl io::Write for CapturedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .bytes
            .lock()
            .expect("capture lock")
            .extend_from_slice(buf);
        self.0.changed.notify_all();
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
fn filter_caps_burst_and_sustained_worker_floods_with_context_and_summaries() {
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

    CLOCK_OFFSET_MS.store(120_000, Ordering::SeqCst);
    const SLOW_WORKER_TASK: &str = "log_rate_filter::idle_query";
    // Mimic an unrelated slow-task diagnostic emitted elsewhere in the same
    // process (e.g. another runtime worker): global subscriber capture sees it,
    // so the assertions below must filter on this test's own task marker and
    // callsite line rather than counting every `workers::worker` warning.
    tracing::warn!(
        target: "workers::worker",
        task = "unrelated background task",
        worker = 99,
        elapsed_ms = 25,
        limit_ms = 10,
        "a task held its worker past the limit; move blocking work to spawn_blocking"
    );
    let mut slow_worker_line = 0u32;
    for _ in 0..=120 {
        slow_worker_line = line!() + 1;
        tracing::warn!(
            target: "workers::worker",
            task = SLOW_WORKER_TASK,
            worker = 2,
            elapsed_ms = 13,
            limit_ms = 10,
            "a task held its worker past the limit; move blocking work to spawn_blocking"
        );
        CLOCK_OFFSET_MS.fetch_add(500, Ordering::SeqCst);
    }
    assert!(
        output
            .lines()
            .iter()
            .any(|line| line.contains("task=\"unrelated background task\"")),
        "unrelated worker warning should reach the global subscriber so marker isolation is exercised"
    );
    let worker_lines: Vec<_> = output
        .lines()
        .into_iter()
        .filter(|line| {
            line.contains("a task held its worker past the limit")
                && line.contains(&format!("task=\"{SLOW_WORKER_TASK}\""))
        })
        .collect();
    assert_eq!(worker_lines.len(), 2, "one diagnostic per minute");
    for line in worker_lines {
        for context in [
            format!("task=\"{SLOW_WORKER_TASK}\""),
            "worker=2".to_owned(),
            "elapsed_ms=13".to_owned(),
            "limit_ms=10".to_owned(),
        ] {
            assert!(line.contains(&context), "missing {context}: {line}");
        }
    }
    for _ in 0..10 {
        tracing::warn!(target: "workers::worker", task = "panicked query", worker = 2,
            "a task panicked; its caller gets the error and the worker goes on");
    }
    assert_eq!(
        output
            .lines()
            .iter()
            .filter(|line| line.contains("a task panicked"))
            .count(),
        5,
        "other worker diagnostics retain the ordinary ceiling"
    );

    for _ in 0..10 {
        tracing::error!(target: "workers::worker", task = "failed query", worker = 2,
            elapsed_ms = 13, limit_ms = 10, "worker failure with timings");
        tracing::warn!(target: "different::worker", task = "different task", worker = 2,
            elapsed_ms = 13, limit_ms = 10, "another subsystem diagnostic");
    }
    for message in [
        "worker failure with timings",
        "another subsystem diagnostic",
    ] {
        assert_eq!(
            output
                .lines()
                .iter()
                .filter(|line| line.contains(message))
                .count(),
            5,
            "{message} retains the ordinary ceiling"
        );
    }

    let expected_original_line = format!("original_line={slow_worker_line}");
    let summary_seen = output.wait_for(Duration::from_secs(10), |bytes| {
        let text = String::from_utf8_lossy(bytes);
        text.lines()
            .any(|line| line.contains(SUMMARY_TARGET) && line.contains("suppressed=995"))
            && text.lines().any(|line| {
                line.contains(SUMMARY_TARGET)
                    && line.contains("suppressed=119")
                    && line.contains("original_target=workers::worker")
                    && line.contains("original_file=")
                    && line.contains("log_rate_filter.rs")
                    && line.contains(&expected_original_line)
            })
    });
    assert!(
        summary_seen,
        "suppression summary missing or wrong: {:#?}",
        output.lines()
    );
}
