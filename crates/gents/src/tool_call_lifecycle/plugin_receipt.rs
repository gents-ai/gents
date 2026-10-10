use anyhow::{ensure, Result};

pub(super) fn ensure_commit<T: PartialEq + ?Sized>(
    generation_matches: bool,
    terminal: bool,
    existing: Option<&T>,
    receipt: &T,
) -> Result<()> {
    ensure!(
        generation_matches,
        "plugin receipt has a stale execution generation"
    );
    match existing {
        Some(existing) => ensure!(
            existing == receipt,
            "plugin receipt conflicts with durable evidence"
        ),
        None => ensure!(
            !terminal,
            "terminal tool call cannot acquire a plugin receipt"
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_receipt_commit_cases() {
        for case in crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases
            ["receipt"]
            .as_array()
            .expect("modeled plugin receipt cases")
        {
            let result = super::ensure_commit(
                case["generation_matches"].as_bool().unwrap(),
                case["terminal"].as_bool().unwrap(),
                case["existing"].as_str(),
                case["receipt"].as_str().unwrap(),
            );
            assert_eq!(
                result.is_ok(),
                case["accepted"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::super::*;
    use crate::config_client::ConfigAccess;
    use crate::graphql::escape_graphql_string;
    use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};

    fn receipt() -> PluginExecutionReceipt {
        PluginExecutionReceipt {
            coordinate: "fixture@1".into(),
            artifact_digest: "artifact".into(),
            input_digest: "input".into(),
            output_digest: Some("output".into()),
            authority: None,
            limits: None,
            verdict: PluginExecutionVerdict::Success,
        }
    }

    #[tokio::test]
    async fn plugin_receipt_commits_with_terminal_and_rejects_conflicting_replay() {
        for terminal in ["complete", "fail", "timeout", "cancel"] {
            let (node, path, mut tool) =
                admission_fixture::published_spawn_parent(&format!("plugin-receipt-{terminal}"))
                    .await;
            let doc_id = tool.doc_id().unwrap().to_owned();
            let mut replay = ToolCallLifecycle::load_by_doc_id(
                node.clone(),
                &doc_id,
                &tool.node_did,
                &tool.session_id,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap()
            .unwrap();
            let evidence = receipt();
            tool.stage_plugin_receipt(Some(evidence.clone())).unwrap();
            match terminal {
                "complete" => {
                    tool.complete("model output").await.unwrap();
                }
                "fail" => {
                    tool.fail("model output", FailureClass::External)
                        .await
                        .unwrap();
                }
                "timeout" => {
                    tool.timeout().await.unwrap();
                }
                _ => {
                    tool.cancel_during_run(CancelCause::Interrupted)
                        .await
                        .unwrap();
                }
            }
            let row = ConfigAccess::Local(node.clone()).execute(&format!(
                r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
                escape_graphql_string(&doc_id),
            )).await.unwrap();
            let stored: PluginExecutionReceipt = serde_json::from_value(
                row["data"]["AgentToolCall"][0]["plugin_execution_receipt"].clone(),
            )
            .unwrap();
            assert_eq!(stored, evidence);
            if terminal == "complete" {
                let presented = load_tool_call_presentation(
                    &ConfigAccess::Local(node.clone()),
                    &doc_id,
                    &tool.node_did,
                    &tool.session_id,
                    tool.requester_did.as_deref(),
                )
                .await
                .unwrap();
                assert_eq!(presented.result.as_deref(), Some("model output"));
                replay.stage_plugin_receipt(Some(evidence.clone())).unwrap();
                replay.complete("model output").await.unwrap();
                let mut conflict = ToolCallLifecycle::load_by_doc_id(
                    node.clone(),
                    &doc_id,
                    &tool.node_did,
                    &tool.session_id,
                    tool.requester_did.as_deref(),
                )
                .await
                .unwrap()
                .unwrap();
                conflict.state = ToolCallState::Running;
                let mut changed = evidence;
                changed.input_digest = "conflicting-input".into();
                conflict.stage_plugin_receipt(Some(changed)).unwrap();
                assert!(conflict.complete("model output").await.is_err());
            }
            node.shutdown().await;
            let _ = std::fs::remove_dir_all(path);
        }
    }

    #[tokio::test]
    async fn stale_generation_cannot_publish_plugin_receipt_or_output() {
        let (node, path, mut tool) =
            admission_fixture::published_spawn_parent("plugin-receipt-stale").await;
        tool.stage_plugin_receipt(Some(receipt())).unwrap();
        ConfigAccess::write_local(&node, "test.stale_plugin_generation", &format!(
            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ execution_generation: "replacement" }}) {{ _docID }} }}"#,
            escape_graphql_string(tool.request_doc_id.as_deref().unwrap()),
        )).await.unwrap();
        assert!(tool.complete("must not publish").await.is_err());
        let row = ConfigAccess::Local(node.clone()).execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
            escape_graphql_string(tool.doc_id().unwrap()),
        )).await.unwrap();
        assert_eq!(
            row["data"]["AgentToolCall"][0]["lifecycle_state"],
            "running"
        );
        assert!(row["data"]["AgentToolCall"][0]["plugin_execution_receipt"].is_null());
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
    #[tokio::test]
    async fn competing_terminal_without_receipt_is_never_backfilled() {
        let (node, path, mut winner) =
            admission_fixture::published_spawn_parent("plugin-late-receipt").await;
        let doc_id = winner.doc_id().unwrap().to_owned();
        let mut loser = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &doc_id,
            &winner.node_did,
            &winner.session_id,
            winner.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .unwrap();
        winner
            .cancel_during_run(CancelCause::Interrupted)
            .await
            .unwrap();
        loser.stage_plugin_receipt(Some(receipt())).unwrap();
        loser.complete("late plugin success").await.unwrap();
        assert_eq!(loser.state, ToolCallState::Cancelled);
        let row = ConfigAccess::Local(node.clone()).execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
            escape_graphql_string(&doc_id),
        )).await.unwrap();
        assert_eq!(
            row["data"]["AgentToolCall"][0]["lifecycle_state"],
            "cancelled"
        );
        assert!(row["data"]["AgentToolCall"][0]["plugin_execution_receipt"].is_null());
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}
