use crate::tools::call::lifecycle::admission_fixture::{
    complete_child_managed_verified, published_admission, published_session_message,
    PublishedAdmission, PublishedAdmissionOptions,
};
use gents_protocol::tool_call::AwaitMode;

use super::fixtures::completion_obligations;

async fn kill_fixture(observation: &str) -> (PublishedAdmission, Option<String>) {
    let options = |start_running| PublishedAdmissionOptions {
        name: format!("kill-session-message-{observation}"),
        real_identity: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(crate::tools::native::AGENT_NEW_TOOL_NAME.to_owned()),
        start_running,
        ..Default::default()
    };
    match observation {
        "unresolved" => (published_admission(options(true)).await.unwrap(), None),
        "causedLiveLocal" | "causedTerminal" => {
            let message = published_session_message(options(false)).await.unwrap();
            if observation == "causedTerminal" {
                complete_child_managed_verified(
                    &message.admission.node,
                    &message.caused_request_id,
                    &message.admission.node_did,
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
        other => panic!("unknown Lean kill observation {other}"),
    }
}

#[tokio::test]
async fn generated_kill_cases_drive_the_session_message_kill() {
    use crate::session::message::KillOutcome;
    use crate::tools::call::lifecycle::ToolCallState;
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().session_message_kill_cases;
    assert_eq!(cases.len(), 3);
    for case in cases {
        let (admission, caused_request_id) = kill_fixture(&case.observation).await;
        let node = admission.node.clone();
        let did = admission.node_did.clone();
        let session_id = admission.tool.session_id().to_owned();
        let mut tool = admission.tool;
        let outcome = crate::session::message::kill(&node, &mut tool)
            .await
            .unwrap();
        let (notifications, _) =
            completion_obligations(&tool.access().unwrap(), &session_id, &did, tool.requester_did())
                .await;
        match case.action.as_str() {
            "settle" => {
                assert_eq!(outcome, KillOutcome::Settled, "{case:?}");
                assert_eq!(tool.state(), ToolCallState::Completed, "{case:?}");
                assert_eq!(notifications.len(), 1, "{notifications:?}");
                assert!(notifications[0].contains("finished before the kill"));
            }
            "interruptCaused" => {
                assert_eq!(
                    outcome,
                    KillOutcome::Interrupting {
                        request_id: caused_request_id.unwrap()
                    },
                    "{case:?}"
                );
                assert!(tool.is_running(), "{case:?}");
                assert!(notifications.is_empty(), "{notifications:?}");
            }
            "cancelRow" => {
                assert_eq!(outcome, KillOutcome::Cancelled, "{case:?}");
                assert_eq!(tool.state(), ToolCallState::Cancelled, "{case:?}");
                assert_eq!(notifications.len(), 1, "{notifications:?}");
                assert!(notifications[0].contains("explicit_cancel"));
            }
            other => panic!("unknown Lean kill action {other}"),
        }
        node.shutdown().await.expect("shut down test node");
        std::fs::remove_dir_all(admission.path).unwrap();
    }
}
