use super::{AwaitMode, CancelCause};
use crate::config_client::ConfigAccess;
use crate::tool_call_lifecycle::admission_fixture::{
    published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use defra_node::EmbeddedNode;

async fn row(node: &EmbeddedNode, doc_id: &str) -> serde_json::Value {
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_doc_id await_mode cancel_policy child_request_id lifecycle_state cancel_cause status }} }}"#,
            crate::graphql::escape_graphql_string(doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1, "physical lifecycle identity must be unique");
    rows.into_iter().next().unwrap()
}

async fn teardown(admission: PublishedAdmission) {
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn generated_background_lifecycle_cases_use_canonical_admission_owner() {
    for case in crate::lean_vocab_test::lean_r6_backgrounding_cases()
        .iter()
        .filter(|case| case.group == "native_lifecycle")
    {
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: format!("background-{}", case.name),
            await_mode: if case.action == "background" {
                AwaitMode::Foreground
            } else {
                AwaitMode::Background
            },
            start_running: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let request_doc_id = admission.tool.request_doc_id().unwrap().to_owned();
        match case.action.as_str() {
            "background" => admission.tool.background().await.unwrap(),
            // Lean's abstract `.complete` action is implemented for an
            // accepted background native row by the bridge completion owner;
            // `complete` is intentionally reserved for foreground native
            // calls and rejects this durable Background shape.
            "complete" => assert!(admission
                .tool
                .bridge_complete(case.result.clone().unwrap_or_default())
                .await
                .unwrap()),
            "cancel_during_run" => assert!(admission
                .tool
                .cancel_during_run(CancelCause::UserCancelled)
                .await
                .unwrap()),
            other => panic!("unhandled modeled lifecycle action {other}"),
        }

        let actual = row(&admission.node, &tool_doc_id).await;
        assert_eq!(actual["request_doc_id"], request_doc_id, "{}", case.name);
        assert_eq!(actual["await_mode"], case.await_mode, "{}", case.name);
        assert_eq!(actual["cancel_policy"], case.cancel_policy, "{}", case.name);
        assert_eq!(
            actual["child_request_id"].as_str(),
            case.child_request_id.as_deref(),
            "{}",
            case.name
        );
        assert_eq!(
            actual["lifecycle_state"], case.terminal_state,
            "{}",
            case.name
        );
        if let Some(expected) = case.result.as_deref() {
            let presentation = super::load_tool_call_presentation(
                &ConfigAccess::Local(admission.node.clone()),
                &tool_doc_id,
                &admission.agent_did,
                &format!("session-background-{}", case.name),
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                presentation.result.as_deref(),
                Some(expected),
                "{}",
                case.name
            );
        }
        if case.action == "cancel_during_run" {
            assert_eq!(actual["cancel_cause"], "userCancelled", "{}", case.name);
            assert!(
                actual["status"]
                    .as_str()
                    .is_some_and(|status| status.contains(case.reason.as_deref().unwrap())),
                "{}: canonical cancellation presentation must retain the modeled reason",
                case.name,
            );
        }
        teardown(admission).await;
    }
}
