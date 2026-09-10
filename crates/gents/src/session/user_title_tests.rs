
use super::conversation::apply_title_in_txn;
use super::query::load_agent_session;
use super::*;
use gents_protocol::session::{SessionTitle, SessionTitleSource};

#[tokio::test]
async fn explicit_user_rename_and_clear_preserve_session_and_request_observation() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    node.add_schema(gents_protocol::schemas::AGENT_SESSION)
        .await
        .unwrap();
    let response = node.execute(r#"mutation { create_AgentSession(input: {
            session_id: "title-session", agent_did: "owner", behavior_id: "behavior",
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
        crate::config_client::ConfigAccess::transact_local(
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
    crate::config_client::ConfigAccess::transact_local(&node, None, "test.user_title", |txn| {
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
    })
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
    crate::config_client::ConfigAccess::transact_local(&node, None, "test.clear_title", |txn| {
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
    })
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
    node.shutdown().await;
}

fn model_id(value: &serde_json::Value) -> serde_json::Value {
    if value.is_null() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(value.to_string())
    }
}

fn model_time(value: &serde_json::Value) -> serde_json::Value {
    value
        .as_i64()
        .map(|seconds| {
            serde_json::Value::String(
                chrono::DateTime::from_timestamp(seconds, 0)
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            )
        })
        .unwrap_or(serde_json::Value::Null)
}

/// Translate Lean's interned identities and logical seconds, without deriving
/// any expected title/update outcome outside the Lean transition owner.
fn model_session(value: &serde_json::Value) -> gents_protocol::session::AgentSession {
    let mut provenance = value["provenance"].clone();
    if let Some(object) = provenance.as_object_mut() {
        for name in ["task_id", "graph_run_id", "parent_request_doc_id"] {
            object.insert(name.into(), model_id(&value["provenance"][name]));
        }
        if let Some(fork) = object
            .get_mut("fork")
            .and_then(serde_json::Value::as_object_mut)
        {
            fork.insert(
                "source_session_id".into(),
                model_id(&value["provenance"]["fork"]["source_session_id"]),
            );
        }
    }
    let observation = if value["observation"].is_null() {
        serde_json::Value::Null
    } else {
        let latest = &value["observation"]["latest"];
        serde_json::json!({
            "last_activity_at": model_time(&value["observation"]["activity"]),
            "preview": value["observation"]["preview"],
            "latest_request": if latest.is_null() {serde_json::Value::Null} else {serde_json::json!({
                "request_doc_id": model_id(&latest["doc_id"]), "request_id": model_id(&latest["request_id"]),
                "lifecycle_state": latest["state"],
            })},
        })
    };
    serde_json::from_value(serde_json::json!({
            "session_id": model_id(&value["scope"]["session"]), "agent_did": model_id(&value["scope"]["agent"]),
            "requester_did": model_id(&value["scope"]["requester"]), "behavior_id": model_id(&value["behavior"]),
            "created_at": model_time(&value["created_at"]), "closed_at": model_time(&value["closed_at"]),
            "title": value["title"], "tags": value["tags"], "provenance": provenance, "observation": observation,
        })).unwrap()
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
    assert!(
        cases
            .iter()
            .any(|case| !case["before"]["title"].is_null() && !case["title"].is_null())
    );
    for case in cases {
        let before = model_session(&case["before"]);
        let expected = model_session(&case["after"]);
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema(gents_protocol::schemas::AGENT_SESSION)
            .await
            .unwrap();
        crate::config_client::ConfigAccess::transact_local(&node, None, "test.seed_model_title", |txn| {
                let before = before.clone();
                Box::pin(async move {
                    txn.execute_with_variables(
                        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) {_docID} }",
                        &serde_json::json!({"input":before}),
                    ).await?;
                    Ok(())
                })
            }).await.unwrap();
        let now = model_time(&case["now"]).as_str().unwrap().to_owned();
        let title = case["title"]["text"].as_str();
        crate::config_client::ConfigAccess::transact_local(
            &node,
            None,
            "test.model_user_title",
            |txn| {
                let before = before.clone();
                let now = now.clone();
                Box::pin(async move {
                    apply_title_in_txn(
                        txn,
                        &before.agent_did,
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
            &before.agent_did,
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
