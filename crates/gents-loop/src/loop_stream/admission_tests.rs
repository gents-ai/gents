use super::{AdmissionWork, OwnedLoopItem};
use crate::ToolCallHookAction;
use futures::{stream, StreamExt};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn drive_admissions<S>(
    stream: S,
) -> impl futures::Stream<Item = Result<super::LoopStreamItem, super::LoopStreamError>>
where
    S: futures::Stream<Item = Result<OwnedLoopItem, super::LoopStreamError>>,
{
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    super::drive_admissions(stream, receiver)
}

#[tokio::test]
async fn dropping_the_loop_cancels_pending_compaction() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let marker = Dropped(dropped.clone());
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (reply, result) = tokio::sync::oneshot::channel();
    let inner = async_stream::stream! {
        sender.send(super::CompactionWork {
            future: Box::pin(async move {
                let _marker = marker;
                std::future::pending().await
            }),
            reply,
        }).unwrap();
        let _ = result.await;
        yield Ok(OwnedLoopItem::Output(super::LoopStreamItem::Final { text: "done".into() }));
    };
    let mut stream = Box::pin(super::drive_admissions(inner, receiver));
    assert!(futures::poll!(stream.next()).is_pending());
    drop(stream);
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn dropping_pending_provider_dispatch_keeps_scope_and_cancels_work() {
    use crate::tool_call_lifecycle::runtime::{
        current_tool_runtime_context, scope_request_tool_execution, scope_tool_request_identity,
    };
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    let marker = Dropped(dropped.clone());
    let observed = Arc::new(std::sync::Mutex::new(None));
    let capture = observed.clone();
    let (reply, result) = tokio::sync::oneshot::channel::<()>();
    let inner = async_stream::stream! {
        yield Ok(OwnedLoopItem::ProviderWork(Box::pin(async move {
            let _marker = marker;
            let context = current_tool_runtime_context().expect("request scope");
            *capture.lock().unwrap() = context.request_id;
            std::future::pending::<()>().await;
            let _ = reply.send(());
        })));
    };
    let mut stream = Box::pin(drive_admissions(inner));
    scope_tool_request_identity(
        Some("requester".into()),
        Some("node".into()),
        Some("agent".into()),
        Some("request".into()),
        scope_request_tool_execution(None, Default::default(), async {
            assert!(futures::poll!(stream.next()).is_pending());
            drop(stream);
        }),
    )
    .await;
    assert_eq!(*observed.lock().unwrap(), Some("request".into()));
    assert!(dropped.load(Ordering::SeqCst));
    assert!(result.await.is_err());
}

async fn model_case(name: &'static str) -> serde_json::Value {
    tokio::task::spawn_blocking(move || {
        let json = gents_lean_contract::load_contract_json().expect("Lean contract snapshot");
        let snapshot: serde_json::Value = serde_json::from_str(&json).unwrap();
        snapshot["tool_admission_handoff_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()
            .clone()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn handoff_delivers_one_reply_before_invocation() {
    let model = model_case("accepted-before-invoke").await;
    let invoked = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let work = AdmissionWork {
        future: Box::pin(async { ToolCallHookAction::Continue }),
        reply: sender,
    };
    let seen = invoked.clone();
    let inner = async_stream::stream! {
        yield Ok(OwnedLoopItem::Admission(work));
        if matches!(receiver.await, Ok(ToolCallHookAction::Continue)) {
            seen.store(true, Ordering::SeqCst);
        }
        yield Ok(OwnedLoopItem::Output(super::LoopStreamItem::Final {text: "done".into()}));
    };
    let mut stream = Box::pin(drive_admissions(inner));
    let item = stream.next().await.unwrap().unwrap();
    assert!(matches!(item, super::LoopStreamItem::Final { .. }));
    assert_eq!(invoked.load(Ordering::SeqCst), model["expected_invoked"]);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn cancellation_after_the_reply_prevents_later_invocation() {
    let model = model_case("cancel-after-reply").await;
    let invoked = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    let (resume, resume_rx) = tokio::sync::oneshot::channel::<()>();
    let work = AdmissionWork {
        future: Box::pin(async { ToolCallHookAction::Continue }),
        reply: sender,
    };
    let seen = invoked.clone();
    let inner = async_stream::stream! {
        yield Ok(OwnedLoopItem::Admission(work));
        if resume_rx.await.is_ok() {
            seen.store(true, Ordering::SeqCst);
        }
        yield Ok(OwnedLoopItem::Output(super::LoopStreamItem::Final {text: "done".into()}));
    };
    let mut stream = Box::pin(drive_admissions(inner));
    assert!(futures::poll!(stream.next()).is_pending());
    assert!(futures::poll!(stream.next()).is_pending());
    assert!(
        matches!(receiver.try_recv(), Ok(ToolCallHookAction::Continue)),
        "the handoff reply was delivered before the stop"
    );
    drop(stream);
    assert!(resume.send(()).is_err());
    assert_eq!(invoked.load(Ordering::SeqCst), model["expected_invoked"]);
}

#[tokio::test]
async fn dropping_waiting_handoff_drops_the_hook_without_replaying_it() {
    let model = model_case("cancel-waiting-unknown").await;
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let marker = Dropped(dropped.clone());
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let work = AdmissionWork {
        future: Box::pin(async move {
            let _marker = marker;
            std::future::pending::<ToolCallHookAction>().await
        }),
        reply: sender,
    };
    let inner = stream::iter(vec![Ok(OwnedLoopItem::Admission(work))]);
    let mut stream = Box::pin(drive_admissions(inner));
    assert!(futures::poll!(stream.next()).is_pending());
    drop(stream);
    assert!(dropped.load(Ordering::SeqCst));
    assert!(receiver.await.is_err());
    assert_eq!(model["expected_dispatch"], "unknown");
    assert_eq!(model["expected_invoked"], false);
}

#[tokio::test]
async fn handoff_retains_the_calling_request_scope() {
    use crate::tool_call_lifecycle::runtime::{
        current_tool_runtime_context, scope_request_tool_execution_with_trigger_context,
        scope_tool_request_identity,
    };
    let observed = Arc::new(std::sync::Mutex::new(None));
    let capture = observed.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let work = AdmissionWork {
        future: Box::pin(async move {
            let context = current_tool_runtime_context().expect("tool request scope");
            *capture.lock().unwrap() = Some((
                context.node_did,
                context.agent_did,
                context.requester_did,
                context.request_id,
                context.session_id,
                context.correlation,
            ));
            ToolCallHookAction::Continue
        }),
        reply: sender,
    };
    let inner = async_stream::stream! {
        yield Ok(OwnedLoopItem::Admission(work));
        let _ = receiver.await;
    };
    let mut stream = Box::pin(drive_admissions(inner));
    scope_tool_request_identity(
        Some("requester".into()),
        Some("node".into()),
        Some("selected-agent".into()),
        Some("request".into()),
        scope_request_tool_execution_with_trigger_context(
            None,
            Default::default(),
            None,
            None,
            Some("session".into()),
            Some("correlation".into()),
            Default::default(),
            false,
            async {
                assert!(stream.next().await.is_none());
            },
        ),
    )
    .await;
    assert_eq!(
        *observed.lock().unwrap(),
        Some((
            Some("node".into()),
            Some("selected-agent".into()),
            Some("requester".into()),
            Some("request".into()),
            Some("session".into()),
            Some("correlation".into())
        ))
    );
}

#[tokio::test]
async fn a_closed_reply_channel_stops_the_loop() {
    let model = model_case("dropped-channel").await;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    drop(receiver);
    let work = AdmissionWork {
        future: Box::pin(async { ToolCallHookAction::Continue }),
        reply: sender,
    };
    let inner = stream::iter(vec![Ok(OwnedLoopItem::Admission(work))]);
    let mut stream = Box::pin(drive_admissions(inner));
    let error = stream
        .next()
        .await
        .expect("closed handoff reports an error")
        .unwrap_err();
    assert!(error.to_string().contains("reply receiver closed"));
    assert_eq!(model["expected_invoked"], false);
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn biased_control_stops_a_pending_handoff_before_invocation() {
    for name in [
        "cancel-waiting-unknown",
        "lease-lost-waiting",
        "shutdown-waiting",
        "deadline-waiting",
    ] {
        let model = model_case(name).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let marker = entered.clone();
        let invoked = Arc::new(AtomicBool::new(false));
        let invoked_by_stream = invoked.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let work = AdmissionWork {
            future: Box::pin(async move {
                marker.notify_one();
                std::future::pending::<ToolCallHookAction>().await
            }),
            reply: sender,
        };
        let inner = async_stream::stream! {
            yield Ok(OwnedLoopItem::Admission(work));
            if matches!(receiver.await, Ok(ToolCallHookAction::Continue)) {
                invoked_by_stream.store(true, Ordering::SeqCst);
            }
            yield Ok(OwnedLoopItem::Output(super::LoopStreamItem::Final { text: "done".into() }));
        };
        let mut stream = Box::pin(drive_admissions(inner));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let mut driver = Box::pin(async {
            tokio::select! {
                biased;
                _ = stopped => false,
                item = stream.next() => { assert!(item.is_none()); true },
            }
        });
        tokio::select! {
            () = entered.notified() => {},
            result = &mut driver => panic!("handoff ended before {name}: {result}"),
        }
        stop.send(()).unwrap();
        assert!(!driver.await);
        drop(stream);
        assert_eq!(
            invoked.load(Ordering::SeqCst),
            model["expected_invoked"],
            "{name}"
        );
    }
}

#[tokio::test]
async fn skip_retains_a_hook_effect_without_invoking_the_guest() {
    let model = model_case("skip-with-hook-effect").await;
    let effect = Arc::new(AtomicBool::new(false));
    let effect_by_hook = effect.clone();
    let invoked = Arc::new(AtomicBool::new(false));
    let invoked_by_stream = invoked.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let work = AdmissionWork {
        future: Box::pin(async move {
            effect_by_hook.store(true, Ordering::SeqCst);
            ToolCallHookAction::Skip {
                reason: "hook completed".into(),
            }
        }),
        reply: sender,
    };
    let inner = async_stream::stream! {
        yield Ok(OwnedLoopItem::Admission(work));
        if matches!(receiver.await, Ok(ToolCallHookAction::Continue)) {
            invoked_by_stream.store(true, Ordering::SeqCst);
        }
        yield Ok(OwnedLoopItem::Output(super::LoopStreamItem::Final { text: "done".into() }));
    };
    let mut stream = Box::pin(drive_admissions(inner));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        super::LoopStreamItem::Final { .. }
    ));
    assert_eq!(
        effect.load(Ordering::SeqCst),
        model["expected_effect"] == "committed"
    );
    assert_eq!(invoked.load(Ordering::SeqCst), model["expected_invoked"]);
}
