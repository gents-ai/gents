use super::*;

fn intent() -> ToolControlIntent {
    ToolControlIntent {
        version: 1,
        intent_id: "intent-123".into(),
        client_did: "client".into(),
        node_did: "node".into(),
        agent_did: "agent".into(),
        session_id: "session".into(),
        session_doc_id: "session-doc".into(),
        request_doc_id: "request-doc".into(),
        requester_did: None,
        tool_call_id: "call".into(),
        tool_call_doc_id: "tool-doc".into(),
        action: "job_stop".into(),
        signature: vec![1],
    }
}

async fn answer(status: u16, body: &'static str) -> Result<String, ControlSendError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut chunk = [0_u8; 4096];
        let mut bytes = Vec::new();
        while !bytes.windows(10).any(|window| window == b"intent-123") {
            let count = socket.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0, "request closed before its signed intent arrived");
            bytes.extend_from_slice(&chunk[..count]);
        }
        if status != 0 {
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let request = control_client()
        .await
        .unwrap()
        .post(format!("http://{address}/jobs/control"))
        .timeout(Duration::from_secs(5))
        .json(&intent());
    let result = submit(request, &intent()).await;
    server.await.unwrap();
    result
}

#[tokio::test]
async fn submission_answers_keep_refusal_distinct_from_unconfirmed() {
    assert_eq!(
        answer(202, r#"{"queued":{"intent_doc_id":"doc-1"}}"#)
            .await
            .unwrap(),
        "doc-1"
    );
    assert!(matches!(
        answer(0, "").await,
        Err(ControlSendError::Unconfirmed { intent_id, intent_doc_id: None, .. }) if intent_id == "intent-123"
    ));
    assert!(matches!(
        answer(200, r#"{"applied":{"state":"cancelled","killed_children":0}}"#).await,
        Err(ControlSendError::Unconfirmed { intent_doc_id: None, .. })
    ));
    assert!(matches!(
        answer(503, r#"{"error":"starting"}"#).await,
        Err(ControlSendError::Unconfirmed { source, .. }) if source.to_string().contains("starting")
    ));
    assert!(matches!(
        answer(403, r#"{"error":"no"}"#).await,
        Err(ControlSendError::NotAuthorized)
    ));
    assert!(matches!(
        answer(404, r#"{"error":"missing"}"#).await,
        Err(ControlSendError::NotFound)
    ));
    assert!(matches!(
        answer(409, r#"{"error":"owner unavailable"}"#).await,
        Err(ControlSendError::Refused { status: 409, detail, .. }) if detail == "owner unavailable"
    ));
}

#[tokio::test]
async fn deadline_reports_the_control_reference_without_claiming_rollback() {
    let observed = ObservedIntent {
        intent_id: "intent-123".into(),
        intent_doc_id: "doc-123".into(),
    };
    let error = bounded(&observed, Duration::ZERO, std::future::pending())
        .await
        .unwrap_err();
    assert!(matches!(
        &error,
        ControlSendError::Unconfirmed { intent_id, intent_doc_id: Some(doc), .. }
            if intent_id == "intent-123" && doc == "doc-123"
    ));
    assert!(error.to_string().contains("may still take effect"));
}

#[tokio::test]
async fn anonymous_access_sends_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let signer =
        crate::KeyIdentity::load_or_create(directory.path().join("client.key"), None).unwrap();
    let anonymous = ConfigAccess::graphql("http://127.0.0.1:9/api/v0/graphql");
    assert!(matches!(
        control_job(&anonymous, &signer, "node", "doc", JobControlKind::Stop).await,
        Err(ControlSendError::NotAuthorized)
    ));
}
