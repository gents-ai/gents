use super::*;

#[tokio::test]
async fn goal_slash_controls_use_runtime_owner_without_submitting_inference() {
    let (_dir, service) = test_service().await;
    let new = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({"_meta":{"sessionId":"goal-controls"}}),
        ))
        .await;
    assert!(parse_response(new.response.as_deref().unwrap())
        .get("error")
        .is_none());
    let node = &service.config.node;
    gents::goal::set_goal(
        node,
        &service.config.node_did,
        "goal-controls",
        Some("Finish the feature"),
        None,
        Some(Some(1000)),
    )
    .await
    .unwrap();
    for (command, expected, expected_reply) in [
        ("status", Some("active"), None),
        ("pause", Some("paused"), None),
        (
            "resume",
            Some("paused"),
            Some("gents goal resume-request --session SESSION --from REQUEST_ID"),
        ),
        ("clear", None, None),
    ] {
        let dispatch = service.handle_acp_payload(&request_payload("session/prompt", json!({
            "sessionId":"goal-controls", "prompt":[{"type":"text", "text":format!("/goal {command}"), "meta":{}}]
        }))).await;
        let response = parse_response(dispatch.response.as_deref().unwrap());
        assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
        assert!(dispatch.notifications.iter().any(|line| {
            let update = parse_response(line);
            update["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                && update["params"]["update"]["_meta"]["hostTurn"] == true
        }));
        if let Some(expected_reply) = expected_reply {
            assert!(dispatch.notifications.iter().any(|line| {
                let update = parse_response(line);
                update["params"]["update"]["content"]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(expected_reply))
            }));
        }
        let goal =
            gents::goal::load_canonical_goal(node, &service.config.node_did, "goal-controls")
                .await
                .unwrap();
        assert_eq!(goal.as_ref().map(|goal| goal.status.as_str()), expected);
    }
    let requests = node.execute("{AgentRequest {_docID}}").await;
    ensure_no_errors(&requests, "goal controls do not infer").unwrap();
    assert!(requests.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    let denied = service
        .handle_acp_payload(&request_payload(
            "session/prompt",
            json!({
                "sessionId":"not-attached", "prompt":[{"type":"text", "text":"/goal clear"}]
            }),
        ))
        .await;
    assert!(parse_response(denied.response.as_deref().unwrap())
        .get("error")
        .is_some());
    service.turns.handle_disconnect().await.unwrap();
}

async fn workflow_session(session: &str) -> (tempfile::TempDir, AcpService) {
    let (dir, service) = service_over(true).await;
    let new = service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({"_meta":{"sessionId":session}}),
        ))
        .await;
    assert!(parse_response(new.response.as_deref().unwrap())
        .get("error")
        .is_none());
    (dir, service)
}

async fn host_reply(service: &AcpService, session: &str, text: &str) -> String {
    let dispatch = service
        .handle_acp_payload(&request_payload(
            "session/prompt",
            json!({"sessionId":session, "prompt":[{"type":"text", "text":text, "meta":{}}]}),
        ))
        .await;
    let response = parse_response(dispatch.response.as_deref().unwrap());
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    dispatch
        .notifications
        .iter()
        .map(|line| parse_response(line))
        .find(|update| {
            update["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
                && update["params"]["update"]["_meta"]["hostTurn"] == true
        })
        .map(|update| {
            update["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .unwrap_or_else(|| panic!("{text} produced no host turn"))
}

async fn no_requests_were_submitted(service: &AcpService) {
    let requests = service
        .config
        .node
        .execute(r#"{AgentRequest(filter: {request_id: {_nlike: "request-call-%"}}) {_docID}}"#)
        .await;
    ensure_no_errors(&requests, "workflow controls do not infer").unwrap();
    assert!(requests.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn workflow_runs_overview() {
    use gents_protocol::job::JobState;
    let (_dir, service) = workflow_session("wf-runs").await;
    let node = &service.config.node;
    let did = &*service.config.node_did;
    seed_job_row(
        node,
        did,
        "wf-runs",
        "call-a",
        "alpha",
        "running",
        JobState::Running,
    )
    .await;
    seed_job_row(
        node,
        did,
        "wf-runs",
        "call-b",
        "beta",
        "completed",
        JobState::Completed,
    )
    .await;
    for text in ["/workflow", "/workflow runs"] {
        let reply = host_reply(&service, "wf-runs", text).await;
        assert!(reply.contains("alpha"), "{reply}");
        assert!(reply.contains("wf-runs:call-a"), "{reply}");
        assert!(reply.contains("running"), "{reply}");
        assert!(!reply.contains("beta"), "{reply}");
    }
    no_requests_were_submitted(&service).await;
    service.turns.handle_disconnect().await.unwrap();
}

#[tokio::test]
async fn stop_or_pause_without_a_handle_lists_the_handles() {
    use gents_protocol::job::JobState;
    let (_dir, service) = workflow_session("wf-list").await;
    let node = &service.config.node;
    let did = &*service.config.node_did;
    seed_job_row(
        node,
        did,
        "wf-list",
        "call-a",
        "alpha",
        "running",
        JobState::Running,
    )
    .await;
    for text in ["/workflow stop", "/workflow pause"] {
        let reply = host_reply(&service, "wf-list", text).await;
        assert!(reply.contains("alpha"), "{reply}");
        assert!(reply.contains("wf-list:call-a"), "{reply}");
    }
    no_requests_were_submitted(&service).await;
    service.turns.handle_disconnect().await.unwrap();
}

#[tokio::test]
async fn unknown_handle_replies_c9_code() {
    use gents_protocol::job::JobState;
    let (_dir, service) = workflow_session("wf-unknown").await;
    let node = &service.config.node;
    let did = &*service.config.node_did;
    seed_job_row(
        node,
        did,
        "wf-other",
        "call-x",
        "elsewhere",
        "running",
        JobState::Running,
    )
    .await;
    for text in [
        "/workflow stop nope",
        "/workflow pause nope",
        "/workflow stop elsewhere",
        "/workflow stop wf-other:call-x",
    ] {
        let reply = host_reply(&service, "wf-unknown", text).await;
        assert!(
            reply.contains("workflow_control_unknown_run"),
            "{text}: {reply}"
        );
        assert!(reply.contains("/workflow runs"), "{text}: {reply}");
    }
    no_requests_were_submitted(&service).await;
    service.turns.handle_disconnect().await.unwrap();
}

async fn job_state(
    service: &AcpService,
    session: &str,
    call: &str,
) -> gents_protocol::job::JobState {
    gents::tools::background::job::view::load_job_view(
        &ConfigAccess::local(service.config.node.clone()),
        None,
        gents::tools::background::job::view::JobKey::Address {
            session_id: session.into(),
            tool_call_id: call.into(),
        },
    )
    .await
    .unwrap()
    .state
}

#[tokio::test]
async fn ended_row_replies_not_applicable() {
    use gents_protocol::job::JobState;
    let (_dir, service) = workflow_session("wf-ended").await;
    seed_job_row(
        &service.config.node,
        &service.config.node_did,
        "wf-ended",
        "call-b",
        "beta",
        "completed",
        JobState::Completed,
    )
    .await;
    for text in ["/workflow stop beta", "/workflow pause beta"] {
        let reply = host_reply(&service, "wf-ended", text).await;
        assert!(
            reply.contains("workflow_control_not_applicable"),
            "{text}: {reply}"
        );
        assert!(
            reply.contains("gents job show wf-ended:call-b"),
            "{text}: {reply}"
        );
    }
    assert_eq!(
        job_state(&service, "wf-ended", "call-b").await,
        JobState::Completed
    );
    service.turns.handle_disconnect().await.unwrap();
}

#[tokio::test]
async fn workflow_stop_replies_host_turn() {
    use gents_protocol::job::JobState;
    let (_dir, service) = workflow_session("wf-stop").await;
    seed_job_row(
        &service.config.node,
        &service.config.node_did,
        "wf-stop",
        "call-a",
        "alpha",
        "running",
        JobState::Running,
    )
    .await;
    let reply = host_reply(&service, "wf-stop", "/workflow stop alpha").await;
    assert!(reply.contains("alpha"), "{reply}");
    assert!(reply.contains('0'), "{reply}");
    assert_eq!(
        job_state(&service, "wf-stop", "call-a").await,
        JobState::Cancelled
    );
    service.turns.handle_disconnect().await.unwrap();
}

#[tokio::test]
async fn workflow_pause_replies_host_turn() {
    use gents_protocol::job::JobState;
    let (_dir, service) = workflow_session("wf-pause").await;
    seed_job_row(
        &service.config.node,
        &service.config.node_did,
        "wf-pause",
        "call-a",
        "alpha",
        "running",
        JobState::Running,
    )
    .await;
    let reply = host_reply(&service, "wf-pause", "/workflow pause alpha").await;
    assert!(reply.contains("alpha"), "{reply}");
    assert!(reply.contains("how a paused run is resumed"), "{reply}");
    assert_eq!(
        job_state(&service, "wf-pause", "call-a").await,
        JobState::Paused
    );
    service.turns.handle_disconnect().await.unwrap();
}

#[test]
fn forwarded_verbs_reach_prompt_text() {
    use crate::commands::grok_shim::workflow_command::WorkflowCommand;
    for text in [
        "/workflow research the topic",
        "/workflow resume alpha",
        "/workflow answer alpha yes",
        "/workflow save alpha",
    ] {
        assert_eq!(WorkflowCommand::parse(text), None, "{text}");
        let request = crate::commands::grok_shim::turn::wire::PromptRequest {
            session_id: "s".to_owned(),
            prompt: vec![PromptBlock {
                kind: "text".to_owned(),
                text: text.to_owned(),
                meta: Some(json!({})),
            }],
            prompt_id: None,
            screen_mode: None,
            send_now: false,
            id: None,
        };
        assert_eq!(
            crate::commands::grok_shim::turn::wire::prompt_text(&request),
            text
        );
    }
}
