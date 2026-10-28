use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gents::log_rate::{RateLimitConfig, RateLimitFilter, SUMMARY_TARGET};
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
#[expect(
    clippy::disallowed_methods,
    reason = "synchronous test waiting for the wall-clock summary flush"
)]
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
    for _ in 0..=120 {
        tracing::warn!(
            target: "workers::worker",
            task = "idle query",
            worker = 2,
            elapsed_ms = 13,
            limit_ms = 10,
            "a task held its worker past the limit; move blocking work to spawn_blocking"
        );
        CLOCK_OFFSET_MS.fetch_add(500, Ordering::SeqCst);
    }
    let worker_lines: Vec<_> = output
        .lines()
        .into_iter()
        .filter(|line| line.contains("a task held its worker past the limit"))
        .collect();
    assert_eq!(worker_lines.len(), 2, "one diagnostic per minute");
    for line in worker_lines {
        for context in [
            "task=\"idle query\"",
            "worker=2",
            "elapsed_ms=13",
            "limit_ms=10",
        ] {
            assert!(line.contains(context), "missing {context}: {line}");
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

    let deadline = Instant::now() + Duration::from_secs(10);
    let summary_seen = loop {
        let lines = output.lines();
        if lines
            .iter()
            .any(|line| line.contains(SUMMARY_TARGET) && line.contains("suppressed=995"))
            && lines.iter().any(|line| {
                line.contains(SUMMARY_TARGET)
                    && line.contains("suppressed=119")
                    && line.contains("original_target=workers::worker")
                    && line.contains("original_file=")
                    && line.contains("original_line=")
            })
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
