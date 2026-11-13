use super::{AwaitMode, ToolCallLifecycle};
use crate::call::lifecycle::admission_fixture::{
    published_admission, PublishedAdmission, PublishedAdmissionOptions,
};

/// The admission's Agent owns its request and tool rows; ACP hides them
/// from any other reader.
fn owner(admission: &PublishedAdmission) -> gents_store::config_client::ConfigAccess {
    admission
        .tool
        .access()
        .expect("admission carries its Agent access")
}

async fn update(admission: &PublishedAdmission, collection: &str, doc_id: &str, fields: &str) {
    let response = owner(admission)
        .write(
            "test.recovery_conformance.update",
            &format!(
                r#"mutation {{ update_{collection}(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(doc_id)
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        response["data"][format!("update_{collection}")]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{collection} {doc_id} update must reach its row"
    );
}

async fn row(admission: &PublishedAdmission, doc_id: &str) -> serde_json::Value {
    let response = owner(admission).execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_doc_id lifecycle_state await_mode cancel_cause tool_failure_class }} }}"#, gents_store::graphql::escape_graphql_string(doc_id))).await.unwrap();
    let rows = response["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1);
    rows.into_iter().next().unwrap()
}

async fn teardown(admission: PublishedAdmission) {
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}

type RestartObservation = gents_lean_vocab::families::contracts_restart_native::Output;

/// Reads a restart recovery back as the model's observation. Production
/// records an interruption and a lost process the same way for both
/// await modes; the row's await mode tells which recovery cause applied.
pub(super) fn observed_restart(
    recovered: usize,
    row: &serde_json::Value,
    messages: &[String],
    wakes: &[serde_json::Value],
    session_id: &str,
) -> RestartObservation {
    let state = row["lifecycle_state"].as_str().unwrap();
    let background = row["await_mode"] == "background";
    let (disposition, terminal_state) = match recovered {
        0 => {
            assert_eq!(state, "running");
            ("leave_running", None)
        }
        1 => ("terminalize", Some(state.to_owned())),
        other => panic!("one row recovered {other} times"),
    };
    let cause = match (
        row["cancel_cause"].as_str(),
        row["tool_failure_class"].as_str(),
    ) {
        (None, None) => None,
        (Some("deadline"), Some("external")) => Some("deadlineExceeded"),
        (Some("interrupted"), None) if background => Some("TerminalizeBackgroundedAsInterrupted"),
        (Some("interrupted"), None) => Some("parentInterrupted"),
        (None, Some("external")) if background => Some("processLost"),
        (None, Some("external")) => Some("parentTerminal"),
        other => panic!("unmodeled recovery record {other:?}"),
    };
    let (notification_reason, queue_source, queue_key_prefix) = match (messages, wakes) {
        ([], []) => (None, None, None),
        ([message], [wake]) => {
            let reason = message
                .split_once("<reason>")
                .and_then(|(_, rest)| rest.split_once("</reason>"))
                .map(|(reason, _)| reason.to_owned())
                .expect("restart notification names its reason");
            let key = wake["queue"]["key"].as_str().unwrap();
            (
                Some(reason),
                wake["queue"]["source"].as_str().map(str::to_owned),
                Some(
                    key.strip_suffix(session_id)
                        .expect("restart wake is keyed by its session")
                        .to_owned(),
                ),
            )
        }
        other => panic!("unmodeled restart obligations {other:?}"),
    };
    RestartObservation {
        disposition: disposition.to_owned(),
        cause: cause.map(str::to_owned),
        terminal_state,
        notification_reason,
        queue_source,
        queue_key_prefix,
    }
}

async fn restart_obligations(
    admission: &PublishedAdmission,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let access = owner(admission);
    let session_id = gents_store::graphql::escape_graphql_string(session_id);
    let data = access
        .execute(&format!(
            r#"{{
                AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }}
                AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ input }}
            }}"#
        ))
        .await
        .unwrap()["data"]
        .clone();
    let mut messages = Vec::new();
    for row in data["AgentMessage"].as_array().unwrap() {
        let doc_id = row["_docID"].as_str().unwrap();
        let (_, message) = gents_session::session::load_canonical_message(
            &access,
            doc_id,
            node_did,
            requester_did,
        )
        .await
        .expect("reconstruct canonical restart notification");
        if let gents_inference::llm::message::Message::User { content } = message {
            for item in content {
                if let gents_inference::llm::message::UserContent::Text(text) = item {
                    if text.text.contains("<tool-completion") {
                        messages.push(text.text);
                    }
                }
            }
        }
    }
    let wakes = data["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["input"].clone())
        .collect();
    (messages, wakes)
}

/// Restarts a running native row through the canonical recovery owner and
/// reads its recovery back; a replay must change nothing.
#[cfg(unix)]
async fn observe_native_restart(
    name: &str,
    await_mode: &str,
    deadline_expired: bool,
    parent_observation: &str,
    process_outcome: &str,
) -> RestartObservation {
    let admission = published_admission(PublishedAdmissionOptions {
        name: format!("restart-{name}"),
        real_identity: true,
        await_mode: AwaitMode::from_persisted(await_mode).expect("modeled await mode"),
        start_running: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let tool_doc = admission.tool.doc_id().unwrap().to_owned();
    let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
    if deadline_expired {
        update(
            &admission,
            "AgentToolCall",
            &tool_doc,
            r#"deadline_at: "2020-01-01T00:00:00Z""#,
        )
        .await;
    }
    match parent_observation {
        "live" => {}
        "interrupted" => {
            update(
                &admission,
                "AgentRequest",
                &request_doc,
                r#"lifecycle_state: "interrupted""#,
            )
            .await;
        }
        "otherTerminal" => {
            update(
                &admission,
                "AgentRequest",
                &request_doc,
                r#"lifecycle_state: "failed""#,
            )
            .await;
        }
        other => panic!("unsupported native restart parent observation {other}"),
    }

    let records = tempfile::tempdir().unwrap();
    let registry = crate::call::hook::BackgroundExecutionRegistry::default()
        .with_process_records(records.path().to_path_buf());
    let process = if await_mode == "background" {
        crate::call::hook::tests::process_records::process_for_generated_outcome(
            &registry,
            admission.tool.tool_call_id(),
            &tool_doc,
            process_outcome,
        )
        .await
    } else {
        None
    };
    let report = ToolCallLifecycle::recover_all_with_executions(
        &admission.node,
        &admission.node_did,
        &super::recovery_closeout_conformance::recovery_identities(&admission),
        &registry,
    )
    .await
    .unwrap();
    if let Some(process) = process {
        assert_ne!(
            process.identity.observe(),
            gents_session::exec::ownership::ProcessObservation::Running,
            "{name}: restart recovery left the recorded process running"
        );
        process.finish().await;
    }
    let actual = row(&admission, &tool_doc).await;
    let session_id = format!("session-restart-{name}");
    let (messages, wakes) = restart_obligations(
        &admission,
        &session_id,
        &admission.node_did,
        admission.tool.requester_did(),
    )
    .await;
    if messages.is_empty() {
        assert_eq!(report.notifications_repaired, 0, "{name}");
    }
    let observed = observed_restart(
        report.tool_calls_recovered,
        &actual,
        &messages,
        &wakes,
        &session_id,
    );
    let second = ToolCallLifecycle::recover_all_with_executions(
        &admission.node,
        &admission.node_did,
        &super::recovery_closeout_conformance::recovery_identities(&admission),
        &registry,
    )
    .await
    .unwrap();
    assert_eq!(second.tool_calls_recovered, 0, "{name}");
    let (messages_after, wakes_after) = restart_obligations(
        &admission,
        &session_id,
        &admission.node_did,
        admission.tool.requester_did(),
    )
    .await;
    assert_eq!(messages_after, messages, "{name}");
    assert_eq!(wakes_after, wakes, "{name}");
    teardown(admission).await;
    observed
}

#[cfg(unix)]
#[tokio::test]
async fn generated_native_restart_dispositions_use_canonical_admission_owner() {
    for case in gents_lean_vocab::families::contracts_restart_native::cases::<String, String>() {
        let input = &case.input;
        let observed = observe_native_restart(
            &input.name,
            &input.await_mode,
            input.deadline_expired,
            &input.parent_observation,
            &input.process_outcome,
        )
        .await;
        case.check(observed);
    }
}

/// The session key of a restart wake is the runtime's session id; the model
/// names it by the input's key.
#[cfg(unix)]
#[tokio::test]
async fn generated_r6_restart_terminalizes_through_the_recovery_owner() {
    for case in gents_lean_vocab::families::contracts_r6_restart::cases() {
        let input = &case.input;
        let observed = observe_native_restart(
            &input.name,
            &input.await_mode,
            false,
            &input.parent_observation,
            &input.process_outcome,
        )
        .await;
        case.check(
            gents_lean_vocab::families::contracts_r6_restart::RestartOutput {
                action: observed.cause.unwrap_or_default(),
                terminal_state: observed
                    .terminal_state
                    .unwrap_or_else(|| "running".to_owned()),
                reason: observed.notification_reason,
                queue_source: observed.queue_source,
                queue_key: observed
                    .queue_key_prefix
                    .map(|prefix| format!("{prefix}{}", input.session_key)),
            },
        );
    }
}

#[tokio::test]
async fn generated_session_message_restart_dispositions_use_canonical_admission_owner() {
    for case in
        gents_lean_vocab::families::contracts_restart_session_message::cases::<String, String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
        let admission = crate::call::lifecycle::admission_fixture::published_session_message(
            PublishedAdmissionOptions {
                name: format!("restart-{name}"),
                real_identity: true,
                await_mode: AwaitMode::from_persisted(&input.await_mode)
                    .expect("modeled await mode"),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .admission;
        let tool_doc = admission.tool.doc_id().unwrap().to_owned();
        let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        if input.deadline_expired {
            update(
                &admission,
                "AgentToolCall",
                &tool_doc,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let parent_state = match input.parent_observation.as_str() {
            "live" => None,
            "interrupted" => Some("interrupted"),
            "cleanlyCompleted" => Some("completed"),
            "otherTerminal" => Some("failed"),
            other => panic!("unsupported session-message restart parent observation {other}"),
        };
        if let Some(state) = parent_state {
            update(
                &admission,
                "AgentRequest",
                &request_doc,
                &format!(r#"lifecycle_state: "{state}""#),
            )
            .await;
            let periodic = ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
                &admission.node,
                &admission.node_did,
                &super::recovery_closeout_conformance::recovery_identities(&admission),
            )
            .await
            .unwrap();
            assert_eq!(periodic.tool_calls_terminalized, 0, "{name}");
        }

        let report = ToolCallLifecycle::recover_all(
            &admission.node,
            &admission.node_did,
            &super::recovery_closeout_conformance::recovery_identities(&admission),
        )
        .await
        .unwrap();
        let actual = row(&admission, &tool_doc).await;
        assert_eq!(actual["await_mode"], input.await_mode.as_str(), "{name}");
        let session_id = format!("session-restart-{name}");
        let (messages, wakes) = restart_obligations(
            &admission,
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        let observed = observed_restart(
            report.tool_calls_recovered,
            &actual,
            &messages,
            &wakes,
            &session_id,
        );
        let second = ToolCallLifecycle::recover_all(
            &admission.node,
            &admission.node_did,
            &super::recovery_closeout_conformance::recovery_identities(&admission),
        )
        .await
        .unwrap();
        assert_eq!(second.tool_calls_recovered, 0, "{name}");
        teardown(admission).await;
        case.check(gents_lean_vocab::family::recode(&observed));
    }
}

#[tokio::test]
async fn generated_native_recovery_cases_use_canonical_admission_owner() {
    let cases = gents_lean_vocab::lean_recovery_sweep_cases();
    for name in [
        "tool_backgrounded_running_unowned_process_to_failed",
        "tool_running_deadline_exceeded_to_timed_out",
        "tool_running_parent_interrupted_to_cancelled",
        "live_running_native_tool_parent_interrupted_to_cancelled",
        "tool_running_terminal_parent_to_failed",
        "live_running_tool_parent_terminal_to_failed",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("recovery-{name}"),
            await_mode: if name.starts_with("tool_backgrounded") {
                AwaitMode::Background
            } else {
                AwaitMode::Foreground
            },
            start_running: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc = admission.tool.doc_id().unwrap().to_owned();
        let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        let deadline_expired = name == "tool_running_deadline_exceeded_to_timed_out";
        let parent_interrupted = matches!(
            name,
            "tool_running_parent_interrupted_to_cancelled"
                | "live_running_native_tool_parent_interrupted_to_cancelled"
        );
        let parent_terminal = matches!(
            name,
            "tool_running_terminal_parent_to_failed"
                | "live_running_tool_parent_terminal_to_failed"
        );
        if deadline_expired {
            update(
                &admission,
                "AgentToolCall",
                &tool_doc,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        if parent_interrupted {
            update(
                &admission,
                "AgentRequest",
                &request_doc,
                r#"lifecycle_state: "interrupted""#,
            )
            .await;
        } else if parent_terminal {
            update(
                &admission,
                "AgentRequest",
                &request_doc,
                r#"lifecycle_state: "completed""#,
            )
            .await;
        }
        let report = ToolCallLifecycle::recover_all(
            &admission.node,
            &admission.node_did,
            &super::recovery_closeout_conformance::recovery_identities(&admission),
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_recovered, 1, "{name}");
        let actual = row(&admission, &tool_doc).await;
        assert_eq!(actual["request_doc_id"], request_doc, "{name}");
        assert_eq!(actual["lifecycle_state"], case.terminal_state, "{name}");
        if parent_interrupted {
            assert_eq!(actual["cancel_cause"], "interrupted", "{name}");
            assert!(actual["tool_failure_class"].is_null(), "{name}");
        } else if name.starts_with("tool_backgrounded") || deadline_expired || parent_terminal {
            assert_eq!(actual["tool_failure_class"], "external", "{name}");
        }
        teardown(admission).await;
    }
}
