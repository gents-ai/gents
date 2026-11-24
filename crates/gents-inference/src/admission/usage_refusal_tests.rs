use super::controller::{InferenceCallRecord, PendingCallMetadata};
use super::persistence::{persist_call_started, persist_existing_call_terminal};
use gents_loop::rig_compat::CachedInputTokensObservation;

/// The refusal xAI returns once a free tier's allowance is spent, as the
/// provider error reaches the admission permit.
const XAI_FREE_TIER_REFUSAL: &str = "ProviderError: You've used all the included free usage for model grok-4.7 for now. Usage resets over a rolling 24-hour window — tokens (actual/limit): 691791/600000. Upgrade to a Grok subscription for higher limits: https://grok.com/supergrok";

/// A failed call's recorded verdict and statement reach the Agent that made
/// it through its own signed read, as the permit recorded them.
#[tokio::test]
async fn an_agent_reads_its_recorded_refusal_as_typed_fields() {
    let (db, signer) = super::tests::test_node().await;
    let node = db.node.clone();
    let request = super::tests::seeded_request(&node, &signer, "usage-refused").await;
    let call = InferenceCallRecord::without_controller(PendingCallMetadata {
        agent_signer: Some(signer.clone()),
        call_id: "usage-refused-call".into(),
        runtime_instance_id: "usage-refusal-runtime".into(),
        request_id: request.request_id.clone(),
        request_doc_id: request.doc_id.clone(),
        request_purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        session_id: request.session_id.clone(),
        requester_did: request.requester_did.clone(),
        model_name: Some("grok-4.7".into()),
        profile_id: Some("grok".into()),
        max_rate_limit_wait: gents_loop::provider_limit::LONG_RATE_LIMIT_WAIT,
        call_seq: 1,
        backend_id: "xai".into(),
        agent_did: signer.did().into(),
        node_did: signer.node_did().into(),
        call_kind: gents_loop::provider_call::admission::CallKind::Inference,
        attempt: 1,
    });
    let now = chrono::Utc::now();
    let (reason, recorded) = super::permit::recorded_failure(
        XAI_FREE_TIER_REFUSAL,
        now,
        gents_loop::provider_limit::LONG_RATE_LIMIT_WAIT,
    );
    let recorded = recorded.expect("the refusal is a provider limit");
    let doc_id = persist_call_started(node.clone(), &call).await.unwrap();
    persist_existing_call_terminal(
        node.clone(),
        &call,
        &doc_id,
        "failed",
        Some(&reason),
        Some(recorded),
        None,
        CachedInputTokensObservation::NotAvailable,
        None,
    )
    .await
    .unwrap();

    let access = gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        identity::Did::new(signer.did()).unwrap(),
        signer.clone(),
    )
    .unwrap();
    let profile = gents_documents::InferenceProfile {
        profile_id: "grok".into(),
        backend_id: "xai".into(),
        model_name: "grok-4.7".into(),
        ..Default::default()
    };
    let refusals = crate::usage_refusal::standing_refusals(
        &access,
        signer.node_did(),
        std::slice::from_ref(&profile),
        now,
    )
    .await
    .unwrap();
    let [refusal] = refusals.as_slice() else {
        panic!("one standing refusal, got {refusals:?}");
    };
    assert_eq!(refusal.verdict, recorded.verdict);
    assert_eq!(refusal.resets_at, recorded.resets_at);
    assert_eq!(refusal.statement, recorded.statement);
    assert_eq!(refusal.profiles, vec!["grok".to_string()]);
    assert_eq!(refusal.agent_did.as_deref(), Some(signer.did()));
}
