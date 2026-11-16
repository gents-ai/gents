use crate::native::command_observability::*;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const CAPTURED_EVENT_TARGETS: &[&str] = &[
    gents_store::config_client::write_telemetry::WRITE_ATTEMPT_EVENT_TARGET,
    gents_session::session::REFUSAL_PROJECTION_EVENT_TARGET,
];

fn enable_scoped_event_capture() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing::subscriber::set_global_default(CapturedTargetSubscriber)
            .expect("no other global tracing default in this test binary");
    });
}

struct CapturedTargetSubscriber;

impl tracing::Subscriber for CapturedTargetSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        CAPTURED_EVENT_TARGETS.contains(&metadata.target())
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[test]
fn credential_failure_is_visible_at_error_level_and_rate_limited() {
    enable_scoped_event_capture();
    let counter = AtomicU64::new(0);
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::ERROR)
        .with_writer(output.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..5 {
            record_credential_failure(&counter, Some("did:key:test-node"));
        }
    });
    let output = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    assert_eq!(counter.load(Ordering::Relaxed), 5);
    assert_eq!(output.matches("credential_inspection_failed").count(), 3);
    assert!(output.contains("did:key:test-node"));
    for count in [1, 2, 4] {
        assert!(
            output.contains(&format!("failure_count={count}")),
            "{output}"
        );
    }
}

#[test]
fn refused_command_updates_policy_counter_without_logging_its_arguments() {
    enable_scoped_event_capture();
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::ERROR)
        .with_writer(output.clone())
        .finish();
    let before = snapshot().policy_rejections;
    tracing::subscriber::with_default(subscriber, || {
        let policy = crate::native::CommandExecutionPolicy::read_only(vec!["ls".into()]);
        assert!(crate::native::shared::validate_command_policy(
            "private-command",
            &["sensitive-argument".into()],
            &policy,
        )
        .is_err());
    });
    assert!(snapshot().policy_rejections > before);
    let output = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    assert!(!output.contains("private-command"));
    assert!(!output.contains("sensitive-argument"));
}
