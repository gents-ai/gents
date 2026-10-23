use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};

use super::ToolCallState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptDecision {
    Commit,
    Adopt,
    Replay,
    Conflict,
}

impl ReceiptDecision {
    pub fn spelling(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Adopt => "adopt",
            Self::Replay => "replay",
            Self::Conflict => "conflict",
        }
    }
}

pub(super) fn published(
    terminal: ToolCallState,
    receipt: &PluginExecutionReceipt,
) -> PluginExecutionReceipt {
    let mut receipt = receipt.clone();
    if terminal != ToolCallState::Completed {
        receipt.output_digest = None;
        if receipt.verdict == PluginExecutionVerdict::Success {
            receipt.verdict = if terminal == ToolCallState::TimedOut {
                PluginExecutionVerdict::Timeout
            } else {
                PluginExecutionVerdict::ExecutionError
            };
        }
    }
    receipt
}

pub(crate) fn decide<T>(terminal: bool, existing: Option<&T>, receipt: &T) -> ReceiptDecision
where
    T: PartialEq + ?Sized,
{
    match (terminal, existing) {
        (false, None) => ReceiptDecision::Commit,
        (false, Some(_)) => ReceiptDecision::Conflict,
        (true, None) => ReceiptDecision::Adopt,
        (true, Some(prior)) if prior == receipt => ReceiptDecision::Replay,
        (true, Some(_)) => ReceiptDecision::Conflict,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modeled(value: &serde_json::Value) -> PluginExecutionReceipt {
        PluginExecutionReceipt {
            coordinate: "team/plugin".into(),
            artifact_digest: "sha256:artifact".into(),
            input_digest: value["evidence"].as_str().unwrap().into(),
            output_digest: value["output"].as_str().map(str::to_owned),
            authority: None,
            limits: None,
            verdict: serde_json::from_value(value["verdict"].clone()).unwrap(),
        }
    }

    #[test]
    fn generated_receipt_commit_cases() {
        for case in crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases
            ["receipt"]
            .as_array()
            .expect("modeled plugin receipt cases")
        {
            let terminal = ToolCallState::from_persisted(case["terminal"].as_str().unwrap())
                .expect("modeled terminal");
            let candidate = published(terminal, &modeled(&case["receipt"]));
            assert_eq!(candidate, modeled(&case["published"]), "{case}");
            let existing = (!case["existing"].is_null()).then(|| modeled(&case["existing"]));
            let decision = match case["decision"].as_str().unwrap() {
                "commit" => ReceiptDecision::Commit,
                "adopt" => ReceiptDecision::Adopt,
                "replay" => ReceiptDecision::Replay,
                "conflict" => ReceiptDecision::Conflict,
                other => panic!("unknown modeled decision {other}"),
            };
            assert_eq!(
                decide(
                    case["row_terminal"].as_bool().unwrap(),
                    existing.as_ref(),
                    &candidate
                ),
                decision,
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
            let (fixture, request_owner) =
                crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                    crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                        name: format!("plugin-receipt-{terminal}"),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
                node,
                path,
                mut tool,
                ..
            } = fixture;
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
            tool.stage_plugin_receipt(Some(evidence.clone()));
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
            let row = ConfigAccess::local(node.clone()).execute(&format!(
                r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
                escape_graphql_string(&doc_id),
            )).await.unwrap();
            let stored: PluginExecutionReceipt = serde_json::from_value(
                row["data"]["AgentToolCall"][0]["plugin_execution_receipt"].clone(),
            )
            .unwrap();
            assert_eq!(stored, super::published(tool.state, &evidence));
            assert_eq!(stored.output_digest.is_some(), terminal == "complete");
            if terminal == "complete" {
                let presented = load_tool_call_presentation(
                    &ConfigAccess::local(node.clone()),
                    &doc_id,
                    &tool.node_did,
                    &tool.session_id,
                    tool.requester_did.as_deref(),
                )
                .await
                .unwrap();
                assert_eq!(presented.result.as_deref(), Some("model output"));
                replay.stage_plugin_receipt(Some(evidence.clone()));
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
                conflict.stage_plugin_receipt(Some(changed));
                assert!(conflict.complete("model output").await.is_err());
            }
            drop(request_owner);
            node.shutdown().await;
            let _ = std::fs::remove_dir_all(path);
        }
    }

    #[tokio::test]
    async fn plugin_child_receipt_commits_with_its_own_terminal_presentation() {
        let (fixture, request_owner) =
            crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: "plugin-child-receipt".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: parent,
            ..
        } = fixture;
        let mut child = parent
            .admit_plugin_effect(1, "nested_plugin", "{}")
            .await
            .unwrap();
        child.start_running().await.unwrap();
        let evidence = receipt();
        child.stage_plugin_receipt(Some(evidence.clone()));
        child.complete("child output").await.unwrap();
        let access = ConfigAccess::local(node.clone());
        let rows = access.execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ plugin_execution_receipt terminal_output lifecycle_state }} }}"#,
            escape_graphql_string(child.doc_id().unwrap()),
        )).await.unwrap();
        let row = &rows["data"]["AgentToolCall"][0];
        assert_eq!(row["lifecycle_state"], "completed");
        assert_eq!(
            serde_json::from_value::<PluginExecutionReceipt>(
                row["plugin_execution_receipt"].clone()
            )
            .unwrap(),
            evidence
        );
        assert!(row["terminal_output"].is_object());
        let presentation = load_tool_call_presentation(
            &access,
            child.doc_id().unwrap(),
            child.node_did(),
            child.session_id(),
            child.requester_did(),
        )
        .await
        .unwrap();
        assert_eq!(presentation.result.as_deref(), Some("child output"));
        let parent_row = access.execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ plugin_execution_receipt lifecycle_state }} }}"#,
            escape_graphql_string(parent.doc_id().unwrap()),
        )).await.unwrap();
        assert!(parent_row["data"]["AgentToolCall"][0]["plugin_execution_receipt"].is_null());
        assert_eq!(
            parent_row["data"]["AgentToolCall"][0]["lifecycle_state"],
            "running"
        );
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn replaced_generation_terminalizes_plugin_call_like_any_tool() {
        let (fixture, request_owner) =
            crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: "plugin-receipt-stale".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            mut tool,
            ..
        } = fixture;
        tool.stage_plugin_receipt(Some(receipt()));
        ConfigAccess::write_local(&node, "test.stale_plugin_generation", &format!(
            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ execution_generation: "replacement" }}) {{ _docID }} }}"#,
            escape_graphql_string(tool.request_doc_id.as_deref().unwrap()),
        )).await.unwrap();
        tool.complete("plugin output").await.unwrap();
        let row = ConfigAccess::local(node.clone()).execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
            escape_graphql_string(tool.doc_id().unwrap()),
        )).await.unwrap();
        assert_eq!(
            row["data"]["AgentToolCall"][0]["lifecycle_state"],
            "completed"
        );
        assert_eq!(
            serde_json::from_value::<PluginExecutionReceipt>(
                row["data"]["AgentToolCall"][0]["plugin_execution_receipt"].clone()
            )
            .unwrap(),
            receipt()
        );
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn competing_terminal_without_receipt_is_never_backfilled() {
        let (fixture, request_owner) =
            crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
                crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: "plugin-late-receipt".to_owned(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: mut winner,
            ..
        } = fixture;
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
        loser.stage_plugin_receipt(Some(receipt()));
        loser.complete("late plugin success").await.unwrap();
        assert_eq!(loser.state, ToolCallState::Cancelled);
        let row = ConfigAccess::local(node.clone()).execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state plugin_execution_receipt }} }}"#,
            escape_graphql_string(&doc_id),
        )).await.unwrap();
        assert_eq!(
            row["data"]["AgentToolCall"][0]["lifecycle_state"],
            "cancelled"
        );
        assert!(row["data"]["AgentToolCall"][0]["plugin_execution_receipt"].is_null());
        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}
