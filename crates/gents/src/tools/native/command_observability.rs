use std::sync::atomic::{AtomicU64, Ordering};

static POLICY_REJECTIONS: AtomicU64 = AtomicU64::new(0);
static CREDENTIAL_INSPECTION_FAILURES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub struct CommandPolicyMetrics {
    pub policy_rejections: u64,
    pub credential_inspection_failures: u64,
}

pub fn snapshot() -> CommandPolicyMetrics {
    CommandPolicyMetrics {
        policy_rejections: POLICY_REJECTIONS.load(Ordering::Relaxed),
        credential_inspection_failures: CREDENTIAL_INSPECTION_FAILURES.load(Ordering::Relaxed),
    }
}

pub(super) fn policy_denied() {
    POLICY_REJECTIONS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
pub(super) fn credential_inspection_failed(node_did: Option<&str>) {
    record_credential_failure(&CREDENTIAL_INSPECTION_FAILURES, node_did);
}

#[cfg(any(target_os = "macos", test))]
fn record_credential_failure(counter: &AtomicU64, node_did: Option<&str>) {
    let count = counter.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    if count.is_power_of_two() {
        tracing::error!(
            reason = "credential_inspection_failed",
            node_did = node_did.unwrap_or("unknown"),
            failure_count = count,
            "Bounded command credential inspection failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
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
        crate::test_support::enable_scoped_event_capture();
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
        crate::test_support::enable_scoped_event_capture();
        let output = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::ERROR)
            .with_writer(output.clone())
            .finish();
        let before = snapshot().policy_rejections;
        tracing::subscriber::with_default(subscriber, || {
            let policy = crate::tools::native::CommandExecutionPolicy::read_only(vec!["ls".into()]);
            assert!(crate::tools::native::shared::validate_command_policy(
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
}
