use super::*;
use crate::test_support::read_observation::ReadCapture;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};
use tracing::instrument::WithSubscriber;

#[tokio::test]
#[ignore = "six-second quiet and traffic samples; writes the requested profile artifact"]
async fn background_wait_read_profile() {
    let output = std::env::var_os("GENTS_BACKGROUND_WAIT_PROFILE")
        .expect("set GENTS_BACKGROUND_WAIT_PROFILE to the measurement output path");
    let mut samples = Vec::new();
    for noisy in [false, true] {
        let name = if noisy {
            "profile-noisy-wait"
        } else {
            "profile-quiet-wait"
        };
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (db, hook, session_id, request_id) = setup_hook(
            name,
            registry(vec![Box::new(GatedTool(gate.clone()))], &["gated_tool"]),
        )
        .await;
        let handle = background_tool(&hook, "profile-background", "gated_tool").await;
        let tool_id = handle["tool_call_id"].as_str().unwrap();
        let caller = crate::tools::background::status::ProcessControlScope {
            request_id,
            session_id,
            node_did: db.node_identity.did().to_owned(),
            requester_did: Some(db.node_identity.did().to_owned()),
        };
        let collections =
            crate::collection_ids(&db.node, ["AgentToolCall", "AgentRequest"]).unwrap();
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        let changes = futures::stream::poll_fn(move |context| {
            if !noisy {
                return Poll::Pending;
            }
            interval.poll_tick(context).map(|_| {
                Some(events::DocumentChangeBatch {
                    changes: collections
                        .iter()
                        .map(|collection| events::DocumentChange {
                            collection_id: collection.clone(),
                            doc_id: "unrelated-profile-document".into(),
                            has_local_write: true,
                        })
                        .collect(),
                    resync_required: false,
                    updates: 2,
                })
            })
        });
        let reads = ReadCapture::new("fetch_interrupt_requested_at_scoped");
        let subscriber = reads.dispatcher();
        let now = chrono::Utc::now();
        let wait = hook
            .await_background_tool_observing(
                &caller,
                tool_id,
                now + chrono::Duration::minutes(1),
                now + chrono::Duration::seconds(30),
                changes,
            )
            .with_subscriber(subscriber);
        let started = Instant::now();
        assert!(tokio::time::timeout(Duration::from_secs(6), wait)
            .await
            .is_err());
        let count = reads.count();
        samples.push(
            json!({"unrelated_batches_per_second": if noisy { 10 } else { 0 },
            "elapsed_ms": started.elapsed().as_millis(), "caller_interruption_reads": count}),
        );
        assert!(count > 0, "the profile must observe actual scoped reads");
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let lifecycle = crate::tools::call::lifecycle::ToolCallLifecycle::load(
                    db.node.clone(),
                    &caller.session_id,
                    tool_id,
                )
                .await
                .unwrap()
                .unwrap();
                if lifecycle.is_terminal() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("ending the observation must preserve the running process");
    }
    std::fs::write(output, serde_json::to_vec_pretty(&samples).unwrap()).unwrap();
}
