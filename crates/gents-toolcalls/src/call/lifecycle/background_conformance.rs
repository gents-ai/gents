use super::AwaitMode;
use super::ToolRowKind;
use crate::call::lifecycle::admission_fixture::{
    published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use gents_lean_vocab::lean_r6_backgrounding_case;
use gents_session::session::tool_call_state::CancelCause;
use gents_store::config_client::ConfigAccess;

/// Reads through the owning Agent's access; ACP hides the row from the node.
async fn row(access: &ConfigAccess, doc_id: &str) -> serde_json::Value {
    let response = access
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_doc_id await_mode lifecycle_state cancel_cause status }} }}"#,
            gents_store::graphql::escape_graphql_string(doc_id)
        ))
        .await
        .unwrap();
    let rows = response["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1, "physical lifecycle identity must be unique");
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

#[tokio::test]
async fn generated_background_lifecycle_cases_use_canonical_admission_owner() {
    for case in gents_lean_vocab::lean_r6_backgrounding_cases()
        .iter()
        .filter(|case| case.group == "native_lifecycle")
    {
        let (session_message, plugin_job) = match case.operation.as_deref() {
            Some("sessionMessage") => (true, false),
            Some("pluginJob") => (false, true),
            Some("nativeCommand") => (false, false),
            other => panic!("{}: unhandled modeled tool operation {other:?}", case.name),
        };
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: format!("background-{}", case.name),
            await_mode: if case.action == "background" {
                AwaitMode::Foreground
            } else {
                AwaitMode::Background
            },
            tool_name: session_message
                .then(|| gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.to_owned()),
            job_pin: plugin_job,
            start_running: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let request_doc_id = admission.tool.request_doc_id().unwrap().to_owned();
        match case.action.as_str() {
            "background" => admission.tool.background().await.unwrap(),
            "foreground" => assert_eq!(
                admission.tool.foreground().await.is_ok(),
                case.legal,
                "{}",
                case.name
            ),
            "complete" => assert!(admission
                .tool
                .complete_owned(case.result.as_deref().unwrap_or_default(), None)
                .await
                .unwrap()),
            "cancel_during_run" => assert!(admission
                .tool
                .cancel_during_run(CancelCause::UserCancelled)
                .await
                .unwrap()),
            other => panic!("unhandled modeled lifecycle action {other}"),
        }

        let actual = row(&admission.tool.access().unwrap(), &tool_doc_id).await;
        assert_eq!(actual["request_doc_id"], request_doc_id, "{}", case.name);
        assert_eq!(actual["await_mode"], case.await_mode, "{}", case.name);
        let lifecycle_state = if case.legal {
            case.terminal_state.as_str()
        } else {
            "running"
        };
        assert_eq!(actual["lifecycle_state"], lifecycle_state, "{}", case.name);
        if let Some(expected) = case.result.as_deref() {
            let presentation =
                gents_session::tool_call::query::result::load_tool_call_presentation(
                    &admission.tool.access().unwrap(),
                    &tool_doc_id,
                    &admission.node_did,
                    &format!("session-background-{}", case.name),
                    admission.tool.requester_did(),
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

/// Lean `restartDisposition` leaves a plugin-job row running and
/// `ToolCallContext.Transition.foreground` refuses it, through the decision
/// cores of `ToolRowKind` that sit beside the orphan sweep's
/// `is_background_tool_row` and `ToolCallLifecycle::foreground`, which call
/// them: a plugin-job row is outside the orphan sweep's scope and is never
/// foregrounded. Each row supplies its own kind and await mode.
#[tokio::test]
async fn generated_plugin_job_restart_and_foreground_rows_match_owners() {
    for case in gents_lean_vocab::families::contracts_restart_plugin_job::cases::<String, String>()
    {
        let input = &case.input;
        let name = input.name.as_str();
        let await_mode = AwaitMode::from_persisted(&input.await_mode)
            .unwrap_or_else(|| panic!("{name}: unknown await mode {:?}", input.await_mode));
        assert!(
            !ToolRowKind::PluginJob.in_orphan_sweep_scope(await_mode),
            "{name}: the orphan sweep must never select a plugin-job row"
        );
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-{name}"),
            real_identity: true,
            await_mode,
            job_pin: true,
            start_running: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let access = admission.tool.access().unwrap();
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let request_doc_id = admission.tool.request_doc_id().unwrap().to_owned();
        if input.deadline_expired {
            crate::call::lifecycle::admission_fixture::update(
                &access,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let parent_state = match input.parent_observation.as_str() {
            "live" => None,
            "interrupted" => Some("interrupted"),
            "cleanlyCompleted" => Some("completed"),
            "otherTerminal" => Some("failed"),
            other => panic!("unsupported plugin-job restart parent observation {other}"),
        };
        if let Some(state) = parent_state {
            crate::call::lifecycle::admission_fixture::update_request(
                &access,
                &request_doc_id,
                &format!(r#"lifecycle_state: "{state}""#),
            )
            .await;
        }
        let report = super::ToolCallLifecycle::recover_all(
            &admission.node,
            &admission.node_did,
            &crate::call::lifecycle::admission_fixture::recovery_identities(&admission),
        )
        .await
        .unwrap();
        let escaped = gents_store::graphql::escape_graphql_string(&tool_doc_id);
        let response = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state await_mode cancel_cause tool_failure_class }} }}"#
            ))
            .await
            .unwrap();
        let session_id = admission.tool.session_id().to_owned();
        let (messages, wakes) = crate::call::lifecycle::admission_fixture::completion_obligations(
            &access,
            &session_id,
            &admission.node_did,
            admission.tool.requester_did(),
        )
        .await;
        let observed = super::recovery_conformance::observed_restart(
            report.tool_calls_recovered,
            &response["data"]["AgentToolCall"][0],
            &messages,
            &wakes,
            &session_id,
        );
        teardown(admission).await;
        case.check(gents_lean_vocab::family::recode(&observed));
    }

    let refusal = lean_r6_backgrounding_case("plugin_job_row_refuses_foreground");
    assert_eq!(refusal.group, "native_lifecycle");
    assert_eq!(refusal.action, "foreground");
    assert_eq!(refusal.operation.as_deref(), Some("pluginJob"));
    assert_eq!(refusal.await_mode, "background");
    assert_eq!(refusal.terminal_state, "rejected");
    assert!(!refusal.legal);
    let kind = match refusal.operation.as_deref() {
        Some("pluginJob") => ToolRowKind::PluginJob,
        other => panic!("plugin_job_row_refuses_foreground: unexpected operation {other:?}"),
    };
    assert_eq!(
        kind.may_foreground(),
        refusal.legal,
        "a plugin-job row must refuse the foreground flip"
    );
}
