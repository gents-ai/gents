use std::sync::Arc;
use std::time::Duration;

use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use serde_json::Value;

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::signed_mailbox::{
    prepare_signed_mailbox_turn, wait_for_request_terminal_on, SignedMailboxTurn,
};
use crate::support::streaming_backend::StreamChunk;
use crate::support::test_db;

const BACKEND_ID: &str = "effective-binding-backend";
const MODEL: &str = "effective-binding-model";

async fn request_row(turn: &SignedMailboxTurn, request_id: &str) -> Value {
    let query = format!(
        r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}, limit: 2) {{
            lifecycle_state
            executed_model_name
            executed_reasoning_effort
            executed_service_tier
            executed_inference_profile_id
            executed_runtime_generation
        }}}}"#,
        escape_graphql_string(request_id)
    );
    let response = turn.client.execute(&query).await.unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "request {request_id} is ambiguous or missing"
    );
    rows[0].clone()
}

async fn select_effort_and_tier(turn: &SignedMailboxTurn, effort: &str, tier: &str) {
    let profile_id = format!("{}:profile", turn.agent.did());
    let profile = serde_json::json!({
        "node_did": turn.node_did,
        "profile_id": profile_id,
        "backend_id": BACKEND_ID,
        "model_name": MODEL,
        "reasoning_effort": effort,
        "service_tier": tier,
    });
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceProfile,
        add: profile.clone(),
        update: profile,
    }])
    .unwrap();
    turn.client
        .transact_write("test.effective_binding.profile", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claimed_request_records_the_settings_its_dispatch_bound() {
    let db = test_db("effective-model-binding").await;
    let session_id = "effective-binding-session";
    let request_id = "effective-binding-request";
    let prompt = "effective-binding-prompt";
    let (prepared, turn) = prepare_signed_mailbox_turn(
        &db,
        BACKEND_ID,
        MODEL,
        request_id,
        session_id,
        prompt,
        vec![StreamChunk::text("bound")],
        Vec::new(),
        None,
    )
    .await
    .unwrap();

    let pending = request_row(&turn, request_id).await;
    assert_eq!(pending["lifecycle_state"], "pending");
    for field in [
        "executed_model_name",
        "executed_reasoning_effort",
        "executed_service_tier",
        "executed_inference_profile_id",
        "executed_runtime_generation",
    ] {
        assert!(
            pending[field].is_null(),
            "an undispatched request has no effective {field}: {pending}"
        );
    }

    select_effort_and_tier(&turn, "low", "fast").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let terminal = wait_for_request_terminal_on(&turn.client, request_id, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(terminal, "completed");

    let claimed = request_row(&turn, request_id).await;
    assert_eq!(claimed["executed_model_name"], MODEL);
    assert_eq!(claimed["executed_reasoning_effort"], "low");
    assert_eq!(claimed["executed_service_tier"], "fast");
    assert_eq!(
        claimed["executed_inference_profile_id"],
        format!("{}:profile", turn.agent.did())
    );
    assert!(
        claimed["executed_runtime_generation"]
            .as_u64()
            .is_some_and(|generation| generation >= 1),
        "a claim records the generation that dispatched it: {claimed}"
    );

    let bodies = runtime.backend.observed_completion_bodies();
    assert!(!bodies.is_empty(), "the provider was called");
    for body in &bodies {
        assert_eq!(body["model"], MODEL, "every call names the bound model");
        assert_eq!(
            body["chat_template_kwargs"]["reasoning_effort"], "low",
            "{body}"
        );
        assert_eq!(body["service_tier"], "priority", "{body}");
    }

    let rendered = turn
        .client
        .execute(&format!(
            r#"{{RenderedRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{model_name capture_scope}}}}"#,
            escape_graphql_string(request_id)
        ))
        .await
        .unwrap();
    let rows = rendered["data"]["RenderedRequest"].as_array().unwrap();
    let inference = rows
        .iter()
        .filter(|row| {
            row["capture_scope"]
                .as_str()
                .is_some_and(|scope| scope.starts_with("inference"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        inference.len(),
        bodies.len(),
        "one durable capture per provider call: {rendered}"
    );
    for row in inference {
        assert_eq!(row["model_name"], MODEL);
    }

    runtime.shutdown().await;
}
