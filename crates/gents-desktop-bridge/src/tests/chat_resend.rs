use crate::commands::send_chat_message;
use crate::tests::support::{served_desktop, submit_request};
use crate::types::ChatSendRequest;

fn resend(node_did: &str, session_id: &str, request_id: &str) -> ChatSendRequest {
    ChatSendRequest {
        request_id: Some(request_id.into()),
        client_did: None,
        node_did: node_did.into(),
        agent_did: None,
        session_id: Some(session_id.into()),
        content: String::new(),
        captured_context: None,
        attachments: None,
        delivery: None,
        caused_by_source_doc_id: Some("closed-question".into()),
        answer: Some(gents_protocol::mailbox_question::MailboxQuestionAnswer {
            option_ids: vec!["yes".into()],
            ..Default::default()
        }),
        cwd: Some("/gents-test/removed-chat-folder".into()),
    }
}

#[tokio::test]
async fn a_resend_of_a_landed_request_confirms_before_any_new_send_check() {
    let served = served_desktop().await;
    let node = served.node_did().to_owned();
    submit_request(
        &served.core,
        &served.runtime,
        &served.agent,
        "landed-session",
        "landed",
    )
    .await;

    let confirmed = send_chat_message(&served.core, resend(&node, "landed-session", "landed"))
        .await
        .unwrap();
    assert_eq!(confirmed.request_id, "landed");
    assert_eq!(confirmed.session_id, "landed-session");
    assert_eq!(confirmed.node_did, node);
    assert_eq!(
        confirmed.agent_did.as_deref(),
        Some(served.agent.agent_did.as_str())
    );

    let held = served
        .core
        .send_message_with(
            gents_protocol::operations::request::MessageSendRoute {
                node_did: node.clone(),
                session_id: Some("landed-session".into()),
                request_id: Some("landed".into()),
            },
            async { anyhow::bail!("a held send does not prepare its message") },
        )
        .await
        .unwrap();
    assert_eq!(held.request_id, confirmed.request_id);
    let refused = send_chat_message(&served.core, resend(&node, "landed-session", "new-send"))
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("MailboxItem not found"),
        "a new send still runs its checks: {refused:#}"
    );
    served.shutdown().await;
}
