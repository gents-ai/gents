use super::streaming_backend::{StreamPlan, StreamResponse, StreamScript};
use super::support::{refresh_request_and_assert_fresh_projection, RunningDesktop};
use crate::commands::session_snapshot;
use crate::types::RenderedTimelineItem;

fn held(marker: &'static str, text: &'static str) -> StreamPlan {
    StreamPlan::current_authored_user(
        marker,
        vec![StreamResponse::Stream(StreamScript::paused(marker, [text]))],
    )
}

fn queued(
    content: &'static str,
    after: Option<&'static str>,
) -> impl FnOnce(&mut gents_protocol::request_admission::AgentRequestCreate) {
    move |create| {
        create.content = content.into();
        let mut queue = serde_json::json!({"source": "user", "policy": "append"});
        if let Some(after) = after {
            queue["queued_after_request_id"] = after.into();
        }
        create.input.queue = Some(serde_json::from_value(queue).unwrap());
    }
}

fn steered(
    content: &'static str,
    after: &'static str,
) -> impl FnOnce(&mut gents_protocol::request_admission::AgentRequestCreate) {
    move |create| {
        queued(content, Some(after))(create);
        if let Some(queue) = create.input.queue.as_mut() {
            queue.delivery = gents_protocol::request_input::QueueDelivery::Steer;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_or_folded_submission_keeps_the_running_turns_streaming_text() {
    // Lean `Session.ordinary_queued_input_never_folds`: only an explicit
    // steer folds. `QueuedSteering.user_steer_passes_queued_turns`: the steer
    // reaches the running turn at its next safe boundary, so the head's
    // request supersedes it and keeps streaming, while a later ordinary append
    // waits for its own turn.
    let desktop = RunningDesktop::start(vec![
        held("LIVE-HEAD", "before the steer"),
        held("LIVE-FOLDED", "still streaming"),
    ])
    .await;
    let head = desktop.send("sess_live", "req_head", "LIVE-HEAD").await;
    desktop.wait_for_request_state(&head, "processing").await;
    desktop.provider.wait_for_chunks("LIVE-HEAD", 1).await;
    let folded = desktop
        .send_shaped(
            "sess_live",
            "req_folded",
            steered("LIVE-FOLDED", "req_head"),
        )
        .await;
    desktop.provider.release("LIVE-HEAD");
    let folded_row = desktop.wait_for_request_state(&folded, "superseded").await;
    assert_eq!(folded_row["request_id"], "req_folded");
    desktop
        .wait_for(
            "the steer superseded by the head",
            &format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{folded}" }} }}) {{ superseded_by_request_doc_id }} }}"#
            ),
            |data| data["AgentRequest"][0]["superseded_by_request_doc_id"] == head.as_str(),
        )
        .await;
    desktop
        .wait_for(
            "the head's streaming text",
            "{ AgentOutputSegment { payload request_doc_id } }",
            |data| {
                data["AgentOutputSegment"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["request_doc_id"] == head.as_str()
                            && row["payload"]
                                .as_str()
                                .is_some_and(|payload| payload.contains("still streaming"))
                    })
                })
            },
        )
        .await;
    desktop.wait_for_request_state(&head, "processing").await;
    let queued_doc = desktop
        .send_shaped(
            "sess_live",
            "req_queued",
            queued("LIVE-QUEUED", Some("req_head")),
        )
        .await;
    desktop.wait_for_request_state(&queued_doc, "pending").await;

    // Subscription events from the running runtime cannot supersede the
    // reads under test.
    desktop.core.stop_observer_for_test().await;
    let user = desktop.user_did();
    let node = desktop.node_did().to_owned();
    for request_id in ["req_head", "req_queued", "req_folded"] {
        refresh_request_and_assert_fresh_projection(
            &desktop.core,
            &node,
            "sess_live",
            Some(&user),
            request_id,
        )
        .await;
    }

    for submitted in ["req_queued", "req_folded"] {
        let snapshot = session_snapshot(
            &desktop.core,
            "sess_live".into(),
            Some(node.clone()),
            Some(submitted.into()),
            None,
        )
        .await
        .expect("session read")
        .expect("snapshot");
        assert_eq!(
            snapshot.latest_request_id.as_deref(),
            Some("req_head"),
            "{submitted}"
        );
        assert!(
            snapshot.timeline_items.iter().any(|item| matches!(
                item,
                RenderedTimelineItem::LiveAssistant { content, .. }
                    if content.as_deref() == Some("still streaming")
            )),
            "{submitted}: {:?}",
            snapshot.timeline_items
        );
    }
    desktop.shutdown().await;
}
