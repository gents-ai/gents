use crate::background::job::control::{KillAction, KillObservation};
use crate::call::lifecycle::admission_fixture::{
    complete_child_managed_verified, published_admission, published_session_message,
    PublishedAdmission, PublishedAdmissionOptions,
};
use gents_protocol::tool_call::AwaitMode;

use crate::call::lifecycle::admission_fixture::completion_obligations;

async fn kill_fixture(observation: KillObservation) -> (PublishedAdmission, Option<String>) {
    let options = |start_running| PublishedAdmissionOptions {
        name: format!("kill-session-message-{observation:?}"),
        real_identity: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.to_owned()),
        start_running,
        ..Default::default()
    };
    match observation {
        KillObservation::Unresolved => (published_admission(options(true)).await.unwrap(), None),
        KillObservation::CausedLiveLocal | KillObservation::CausedTerminal => {
            let message = published_session_message(options(false)).await.unwrap();
            if observation == KillObservation::CausedTerminal {
                complete_child_managed_verified(
                    &message.admission.node,
                    &message.caused_request_id,
                    "finished before the kill",
                    &message
                        .admission
                        .tool
                        .access()
                        .unwrap()
                        .managed_agent_signer()
                        .unwrap(),
                    message.admission.runtime.clone(),
                )
                .await;
            }
            (message.admission, Some(message.caused_request_id))
        }
    }
}

#[tokio::test]
async fn generated_kill_cases_drive_the_session_message_kill() {
    use crate::background::caused::KillOutcome;
    use gents_session::session::tool_call_state::ToolCallState;
    for case in gents_lean_vocab::families::session_message_kill_drive::cases::<
        KillAction,
        KillObservation,
    >() {
        let (admission, caused_request_id) = kill_fixture(case.input.observation).await;
        let node = admission.node.clone();
        let did = admission.node_did.clone();
        let session_id = admission.tool.session_id().to_owned();
        let mut tool = admission.tool;
        let outcome = crate::background::caused::kill(
            &node,
            &gents_session::session::message::NoSessionMessageGoals,
            &mut tool,
        )
        .await
        .unwrap();
        let (notifications, _) = completion_obligations(
            &tool.access().unwrap(),
            &session_id,
            &did,
            tool.requester_did(),
        )
        .await;
        let action = match &outcome {
            KillOutcome::Settled => {
                assert_eq!(tool.state(), ToolCallState::Completed);
                assert_eq!(notifications.len(), 1, "{notifications:?}");
                assert!(notifications[0].contains("finished before the kill"));
                KillAction::Settle
            }
            KillOutcome::Interrupting { request_id } => {
                assert_eq!(Some(request_id), caused_request_id.as_ref());
                assert!(notifications.is_empty(), "{notifications:?}");
                KillAction::InterruptCaused
            }
            KillOutcome::Cancelled => {
                assert_eq!(tool.state(), ToolCallState::Cancelled);
                assert_eq!(notifications.len(), 1, "{notifications:?}");
                assert!(notifications[0].contains("explicit_cancel"));
                KillAction::CancelRow
            }
        };
        case.check(
            gents_lean_vocab::families::session_message_kill_drive::DriveOutput {
                action,
                ends_row_now: !tool.is_running(),
            },
        );
        node.shutdown().await.expect("shut down test node");
        std::fs::remove_dir_all(admission.path).unwrap();
    }
}
