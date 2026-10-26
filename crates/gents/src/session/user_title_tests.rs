use std::sync::Arc;

use super::model_test_support::{model_session, model_time};
use super::observations::apply_title_in_txn;
use super::query::load_agent_session;
use super::*;
use gents_protocol::session::{SessionTitle, SessionTitleSource};

#[tokio::test]
async fn title_transitions_preserve_session_and_request_observation() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema(gents_protocol::schemas::AGENT_SESSION)
        .await
        .unwrap();
    let response = node.execute(r#"mutation { create_AgentSession(input: {
            session_id: "title-session", node_did: "owner", agent_id: "agent",
            created_at: "2026-01-01T00:00:00Z", closed_at: "2026-01-02T00:00:00Z",
            title: {text: "task title", source: "task"}, tags: ["important"],
            provenance: {task_id: "task"},
            observation: {last_activity_at: "2026-01-03T00:00:00Z", preview: "keep this preview",
                latest_request: {request_doc_id: "physical-request", request_id: "request", lifecycle_state: "completed"}}
        }) {_docID} }"#).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let before = load_agent_session(&node, "owner", "title-session", None)
        .await
        .unwrap()
        .unwrap();
    for source in [
        SessionTitleSource::Generated,
        SessionTitleSource::Task,
        SessionTitleSource::Placeholder,
    ] {
        crate::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "test.automatic_title",
            |txn| {
                Box::pin(async move {
                    apply_title_in_txn(
                        txn,
                        "owner",
                        None,
                        "title-session",
                        Some("automatic"),
                        source,
                        "2026-01-04T00:00:00Z",
                    )
                    .await
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(
            load_agent_session(&node, "owner", "title-session", None)
                .await
                .unwrap()
                .unwrap(),
            before
        );
    }
    crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.user_title",
        |txn| {
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    "owner",
                    None,
                    "title-session",
                    Some("  renamed  "),
                    SessionTitleSource::User,
                    "2026-01-02T00:00:00Z",
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    let renamed = load_agent_session(&node, "owner", "title-session", None)
        .await
        .unwrap()
        .unwrap();
    let mut expected = before.clone();
    expected.title = Some(SessionTitle {
        text: "renamed".into(),
        source: SessionTitleSource::User,
    });
    assert_eq!(renamed, expected);
    crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.clear_title",
        |txn| {
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    "owner",
                    None,
                    "title-session",
                    None,
                    SessionTitleSource::User,
                    "2026-01-04T00:00:00Z",
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    expected.title = None;
    expected.observation.as_mut().unwrap().last_activity_at = "2026-01-04T00:00:00Z".into();
    assert_eq!(
        load_agent_session(&node, "owner", "title-session", None)
            .await
            .unwrap()
            .unwrap(),
        expected
    );

    let response = node.execute(r#"mutation { create_AgentSession(input: {
            session_id: "generated-title-session", node_did: "owner", agent_id: "agent",
            created_at: "2026-01-01T00:00:00Z", closed_at: "2026-01-02T00:00:00Z",
            title: {text: "New session", source: "placeholder"}, tags: ["important"],
            provenance: {task_id: "task"},
            observation: {last_activity_at: "2026-01-03T00:00:00Z", preview: "keep this preview",
                latest_request: {request_doc_id: "physical-request", request_id: "request", lifecycle_state: "completed"}}
        }) {_docID} }"#).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let placeholder = load_agent_session(&node, "owner", "generated-title-session", None)
        .await
        .unwrap()
        .unwrap();
    crate::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.generated_title",
        |txn| {
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    "owner",
                    None,
                    "generated-title-session",
                    Some("generated title"),
                    SessionTitleSource::Generated,
                    "2026-01-04T00:00:00Z",
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    let generated = load_agent_session(&node, "owner", "generated-title-session", None)
        .await
        .unwrap()
        .unwrap();
    let mut expected_generated = placeholder;
    expected_generated.title = Some(SessionTitle {
        text: "generated title".into(),
        source: SessionTitleSource::Generated,
    });
    expected_generated
        .observation
        .as_mut()
        .unwrap()
        .last_activity_at = "2026-01-04T00:00:00Z".into();
    assert_eq!(generated, expected_generated);
    node.shutdown().await;
}

#[tokio::test]
async fn generated_user_title_cases_drive_the_shared_session_owner() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .projection;
    let cases = cases
        .iter()
        .filter(|case| case["operation"] == "rename")
        .collect::<Vec<_>>();
    assert!(cases.iter().any(|case| case["title"].is_null()));
    assert!(cases
        .iter()
        .any(|case| !case["before"]["title"].is_null() && !case["title"].is_null()));
    for case in cases {
        let before = model_session(&case["before"]);
        let expected = model_session(&case["after"]);
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_schema(gents_protocol::schemas::AGENT_SESSION)
            .await
            .unwrap();
        crate::config_client::ConfigAccess::transact_local_write(&node, None, "test.seed_model_title", |txn| {
                let before = before.clone();
                Box::pin(async move {
                    txn.execute_with_variables(
                        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) {_docID} }",
                        &serde_json::json!({"input":before}),
                    ).await?;
                    Ok::<_, anyhow::Error>(())
                })
            }).await.unwrap();
        let now = model_time(&case["now"]).as_str().unwrap().to_owned();
        let title = case["title"]["text"].as_str();
        crate::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "test.model_user_title",
            |txn| {
                let before = before.clone();
                let now = now.clone();
                Box::pin(async move {
                    apply_title_in_txn(
                        txn,
                        &before.node_did,
                        before.requester_did.as_deref(),
                        &before.session_id,
                        title,
                        SessionTitleSource::User,
                        &now,
                    )
                    .await
                })
            },
        )
        .await
        .unwrap();
        let after = load_agent_session(
            &node,
            &before.node_did,
            &before.session_id,
            before.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(after, expected, "Lean user title case {}", case["name"]);
        node.shutdown().await;
    }
}
