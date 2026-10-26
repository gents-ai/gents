use super::*;
use futures::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

struct ObservedChanges {
    waiting: Arc<tokio::sync::Notify>,
    changes: futures::channel::mpsc::UnboundedReceiver<events::DocumentChangeBatch>,
}

impl Stream for ObservedChanges {
    type Item = events::DocumentChangeBatch;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.waiting.notify_one();
        Pin::new(&mut self.changes).poll_next(context)
    }
}

async fn background_wait_ignores_unrelated_changes(resync_required: bool) {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (db, hook, session_id, request_id) = setup_hook(
        if resync_required {
            "wait-resync"
        } else {
            "wait-document-scope"
        },
        registry(vec![Box::new(GatedTool(gate.clone()))], &["gated_tool"]),
    )
    .await;
    let handle = background_tool(&hook, "scoped-background", "gated_tool").await;
    let tool_id = handle["tool_call_id"].as_str().unwrap();
    let caller = crate::background_tools::ProcessControlScope {
        request_id,
        session_id,
        node_did: db.node_identity.did().to_owned(),
        requester_did: Some(db.node_identity.did().to_owned()),
    };
    let collections = crate::collection_ids(&db.node, ["AgentToolCall", "AgentRequest"]).unwrap();
    let waiting = Arc::new(tokio::sync::Notify::new());
    let (sender, changes) = futures::channel::mpsc::unbounded();
    let changes = ObservedChanges {
        waiting: waiting.clone(),
        changes,
    };
    let now = chrono::Utc::now();
    let wait = hook.await_background_tool_observing(
        &caller,
        tool_id,
        now + chrono::Duration::minutes(1),
        now + chrono::Duration::seconds(30),
        changes,
    );
    tokio::pin!(wait);
    tokio::select! {
        result = &mut wait => panic!("wait returned before the background tool finished: {result:?}"),
        () = waiting.notified() => {}
    }
    gate.add_permits(1);
    let completed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::load(
                db.node.clone(),
                &caller.session_id,
                tool_id,
            )
            .await
            .unwrap()
            .unwrap();
            if lifecycle.is_terminal() {
                break lifecycle;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the gated tool must finish before exercising its invalidation feed");
    let unrelated = || events::DocumentChangeBatch {
        changes: collections
            .iter()
            .map(|collection| events::DocumentChange {
                collection_id: collection.clone(),
                doc_id: "unrelated-physical-document".into(),
                has_local_write: true,
            })
            .collect(),
        resync_required,
        updates: 100,
    };
    sender.unbounded_send(unrelated()).unwrap();
    if !resync_required {
        tokio::select! {
            result = &mut wait => panic!("unrelated changes triggered a durable completion read: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
        sender
            .unbounded_send(events::DocumentChangeBatch {
                changes: vec![events::DocumentChange {
                    collection_id: collections[0].clone(),
                    doc_id: completed.doc_id().unwrap().to_owned(),
                    has_local_write: false,
                }],
                resync_required: false,
                updates: 1,
            })
            .unwrap();
    }
    let result = tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("relevant invalidation or overflow must reread durable completion")
        .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["result"], "wake-completed");
    assert_eq!(result["error"], Value::Null);
}

#[tokio::test]
async fn background_wait_filters_other_requests_and_tools_before_rereading() {
    background_wait_ignores_unrelated_changes(false).await;
}

#[tokio::test]
async fn background_wait_rereads_durable_completion_on_overflow() {
    background_wait_ignores_unrelated_changes(true).await;
}
