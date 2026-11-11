
use gents_tools::native::command_observability::*;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};


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
    crate::test_support::root::enable_scoped_event_capture();
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
    crate::test_support::root::enable_scoped_event_capture();
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::ERROR)
        .with_writer(output.clone())
        .finish();
    let before = snapshot().policy_rejections;
    tracing::subscriber::with_default(subscriber, || {
        let policy = gents_tools::native::CommandExecutionPolicy::read_only(vec!["ls".into()]);
        assert!(gents_tools::native::shared::validate_command_policy(
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
