use super::*;

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
