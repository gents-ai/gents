use gents_desktop_core::error::BridgeErrorCode;
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, ToolControlReceiptOutcome,
};

use super::operations_job_control::{assert_client_signed_intent, control_runtime};

#[tokio::test]
async fn desktop_process_control_publishes_a_client_signed_intent_and_requires_the_agent_receipt() {
    let control = control_runtime().await;
    let command = |doc: &str, call: &str| OperatorProcessControlRequest {
        node_did: control.node_did.clone(),
        session_id: "session".into(),
        tool_call_id: call.into(),
        tool_call_doc_id: doc.into(),
    };

    for (call, outcome) in [
        (
            "cancelled",
            ProcessControlOutcome::Cancelled {
                live_execution_cancelled: true,
            },
        ),
        ("unverified", ProcessControlOutcome::Unverified),
        ("lost", ProcessControlOutcome::Lost),
    ] {
        let doc = control.invocation(call, true).await;
        let answered = control.answer(
            &doc,
            ToolControlReceiptOutcome::Process(outcome.clone()),
            None,
        );
        assert_eq!(
            crate::process_control::process_control(&control.core, &command(&doc, call))
                .await
                .unwrap(),
            outcome
        );
        let intent = answered.await.unwrap();
        assert_client_signed_intent(&control, &intent, &doc, call, "process_stop").await;
    }

    let forged = control.invocation("forged", true).await;
    let answered = control.answer(
        &forged,
        ToolControlReceiptOutcome::Process(ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true,
        }),
        Some(ToolControlReceiptOutcome::Process(
            ProcessControlOutcome::Lost,
        )),
    );
    let error = crate::process_control::process_control(&control.core, &command(&forged, "forged"))
        .await
        .unwrap_err();
    let intent = answered.await.unwrap();
    assert_eq!(error.code, BridgeErrorCode::Unknown);
    assert!(
        error.message.contains("the action may still take effect"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains(&intent.intent_id),
        "{}",
        error.message
    );

    // The displayed logical identity must match the physical row.
    let mismatched = control.invocation("mismatched", true).await;
    let unreadable = control.invocation("unreadable", false).await;
    for request in [
        command(&mismatched, "other-call"),
        command(&unreadable, "unreadable"),
    ] {
        assert_eq!(
            crate::process_control::process_control(&control.core, &request)
                .await
                .unwrap_err()
                .code,
            BridgeErrorCode::NotFound
        );
    }
    control.shutdown().await;
}
