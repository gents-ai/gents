use super::*;

#[derive(Clone, Default)]
struct RetainedRecordEvents(Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>);

impl RetainedRecordEvents {
    fn count(&self, level: tracing::Level) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, message)| *seen == level && message.contains("not proven exited"))
            .count()
    }
}

impl<S> tracing_subscriber::Layer<S> for RetainedRecordEvents
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), message.0));
    }
}

/// A background command that daemonizes a child (`server &`) leaves the child
/// in the call's process group after the leader exits. Recovery revisits the
/// retained record on every sweep; it warns once, then waits at debug until
/// the group exits and the record is forgotten.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn surviving_group_member_warns_once_until_its_group_exits() {
    use crate::tools::exec::ownership::{ProcessIdentity, ProcessObservation, ProcessRecord};
    use std::io::Write as _;
    use std::os::unix::process::CommandExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    let mut leader = std::process::Command::new("sh")
        .args(["-c", "sleep 60 & read _"])
        .stdin(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn group leader");
    let pid = i32::try_from(leader.id()).unwrap();
    let identity = ProcessIdentity::of_live(pid).expect("live leader identity");
    assert_eq!(identity.pgid, pid);
    leader.stdin.take().unwrap().write_all(b"\n").unwrap();
    leader.wait().expect("leader exits");
    assert_eq!(identity.observe(), ProcessObservation::Unowned);

    let registry = BackgroundExecutionRegistry::default();
    registry
        .process_records
        .write(&ProcessRecord {
            tool_call_id: "daemonizing-call".to_owned(),
            tool_call_doc_id: "daemonizing-doc".to_owned(),
            identity: identity.clone(),
        })
        .unwrap();

    crate::test_support::enable_scoped_event_capture();
    let events = RetainedRecordEvents::default();
    let subscriber = tracing_subscriber::Registry::default().with(events.clone());
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..5 {
            registry.forget_process_record("daemonizing-call", "daemonizing-doc");
        }
    });
    assert!(registry
        .process_record("daemonizing-call", "daemonizing-doc")
        .is_some());
    assert_eq!(events.count(tracing::Level::WARN), 1);
    assert_eq!(events.count(tracing::Level::DEBUG), 4);

    // SAFETY: the group is the one this test created; only its member is signalled.
    assert_eq!(unsafe { libc::killpg(identity.pgid, libc::SIGKILL) }, 0);
    assert_eq!(
        identity
            .await_signalled_group_exit(std::time::Duration::from_secs(10))
            .await,
        ProcessObservation::Exited,
        "orphaned member was not reaped"
    );
    registry.forget_process_record("daemonizing-call", "daemonizing-doc");
    assert!(registry
        .process_record("daemonizing-call", "daemonizing-doc")
        .is_none());
    assert!(registry.unproven_record_warnings.lock().unwrap().is_empty());
}
