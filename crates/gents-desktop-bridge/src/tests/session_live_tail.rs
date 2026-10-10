use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
};
use serde_json::Value;

use super::support::{boot_core, OPERATOR};
use crate::tauri_commands::chat::session_snapshot;
use crate::types::RenderedTimelineItem;

async fn create(
    core: &gents_desktop_core::client::ClientCore,
    collection: &str,
    input: Value,
) -> String {
    let mutation = format!(
        "mutation {{ create_{collection}(input: {}) {{ _docID }} }}",
        gents_protocol::graphql::graphql_input_literal(&input).expect("input literal")
    );
    let response = core.node().execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed {collection}: {:?}",
        response.errors
    );
    gents::graphql::single_mutation_document(&response, &format!("create_{collection}"))
        .expect("created")
        .and_then(|document| document["_docID"].as_str())
        .expect("doc id")
        .to_owned()
}

fn request(request_id: &str, state: &str, created_at: &str) -> Value {
    serde_json::json!({
        "purpose": "normal", "request_id": request_id, "node_did": OPERATOR,
        "agent_id": "worker", "session_id": "sess_live",
        "content": format!("{request_id} text"), "lifecycle_state": state, "backend_id": "",
        "created_at": created_at, "retry_count": 0
    })
}

/// H streams; Q was then sent and queued behind it, or F was folded into
/// it. A bounded session read that names Q or F still loads H's open output,
/// so the snapshot keeps H's streaming text as its live tail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_or_folded_submission_keeps_the_running_turns_streaming_text() {
    let (core, _tmp) = boot_core().await;
    let session = gents_protocol::session::AgentSession {
        session_id: "sess_live".into(),
        node_did: OPERATOR.into(),
        requester_did: None,
        agent_id: "worker".into(),
        created_at: "2026-05-20T00:00:00Z".into(),
        closed_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    };
    create(
        &core,
        "AgentSession",
        serde_json::to_value(&session).expect("session json"),
    )
    .await;
    let mut head = request("req_head", "processing", "2026-05-20T00:00:00Z");
    head["execution_generation"] = "gen-1".into();
    head["execution_lease_secs"] = 300.into();
    head["execution_lease_expires_at"] = "2099-01-01T00:00:00Z".into();
    let head_doc = create(&core, "AgentRequest", head).await;
    let queue = serde_json::json!({
        "queue": {"source": "user", "policy": "append", "queued_after_request_id": "req_head"}
    });
    let mut queued = request("req_queued", "pending", "2026-05-20T00:00:01Z");
    queued["input"] = queue.clone();
    create(&core, "AgentRequest", queued).await;
    let mut folded = request("req_folded", "superseded", "2026-05-20T00:00:02Z");
    folded["input"] = queue;
    folded["superseded_by_request"] = "req_head".into();
    folded["superseded_by_request_doc_id"] = head_doc.clone().into();
    folded["failure_reason"] = gents::lifecycle::FOLDED_REASON.into();
    create(&core, "AgentRequest", folded).await;

    let segment = OutputSegment {
        node_did: OPERATOR.into(),
        requester_did: None,
        session_id: "sess_live".into(),
        request_doc_id: head_doc.clone(),
        source: OutputSource::ProviderTurn {
            scope: gents_protocol::rendered_request::CaptureScope {
                kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                seq: 0,
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: "gen-1".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: 15,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: "still streaming".into(),
        close: None,
        created_at: "2026-05-20T00:00:03Z".into(),
    };
    create(
        &core,
        "AgentOutputSegment",
        serde_json::to_value(&segment).expect("segment json"),
    )
    .await;
    for request_id in ["req_head", "req_queued", "req_folded"] {
        core.refresh_local_request(OPERATOR, request_id)
            .await
            .expect("refresh request");
    }

    for submitted in ["req_queued", "req_folded"] {
        let snapshot = session_snapshot(
            &core,
            "sess_live".into(),
            Some(OPERATOR.into()),
            Some(submitted.into()),
            None,
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
}
