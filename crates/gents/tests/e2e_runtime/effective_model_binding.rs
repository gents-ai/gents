use std::sync::Arc;

use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use serde_json::Value;

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::signed_mailbox::{prepare_signed_mailbox_turn, SignedMailboxTurn};
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
    let terminal = wait_for_request_terminal(&turn.client, request_id).await;
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
        assert_eq!(body["reasoning_effort"], "low", "{body}");
        assert!(body.get("chat_template_kwargs").is_none(), "{body}");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_model_line_changes_on_the_next_request_in_the_same_session() {
    use crate::support::live_inference::rows_when;
    use crate::support::streaming_backend::{MockStreamingBackend, StreamScript};
    use gents_protocol::node_readiness::{decode_node_readiness_snapshot, NodeReadinessRow};

    let db = test_db("profile-model-line").await;
    let session = "profile-line-session";
    let (prepared, turn) = prepare_signed_mailbox_turn(
        &db,
        BACKEND_ID,
        MODEL,
        "profile-first",
        session,
        "first-model",
        vec![StreamChunk::text("first")],
    )
    .await
    .unwrap();
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
    assert_eq!(
        wait_for_request_terminal(&turn.client, "profile-first").await,
        "completed"
    );
    let first = runtime.backend.observed_turn_completion_bodies();
    assert_eq!(first.len(), 1);
    assert!(first[0]["messages"]
        .to_string()
        .contains(&format!("Your profile's model: {MODEL}.")));
    let generation = request_row(&turn, "profile-first").await["executed_runtime_generation"]
        .as_u64()
        .unwrap();

    let next_model = "profile-next-model";
    let backend = MockStreamingBackend::start(
        next_model,
        vec![StreamScript::completes("second-model", ["second"])],
    )
    .unwrap();
    let profile = serde_json::json!({
        "node_did":turn.node_did, "profile_id":format!("{}:profile", turn.agent.did()),
        "backend_id":"profile-next-backend", "model_name":next_model,
    });
    let backend_doc = serde_json::json!({
        "node_did":turn.node_did, "backend_id":"profile-next-backend", "name":"profile-next-backend",
        "provider_kind":"OpenAiCompatible", "openai_wire_api":"chat_completions",
        "auth":{"kind":"unauthenticated"}, "endpoint":backend.endpoint(),
        "max_concurrent":1, "max_queue_depth":100, "enabled":true,
    });
    let plan = DesiredStateApplyPlan::new(vec![
        DesiredStateApplyDocument {
            collection: Collection::InferenceBackend,
            add: backend_doc.clone(),
            update: backend_doc,
        },
        DesiredStateApplyDocument {
            collection: Collection::InferenceProfile,
            add: profile.clone(),
            update: profile,
        },
    ])
    .unwrap();
    turn.client
        .transact_write("test.profile_model.change", |txn| {
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
    let query = format!(
        r#"{{NodeReadiness(filter: {{node_did: {{_eq: "{}"}}}}) {{node_did snapshot_json updated_at}}}}"#,
        escape_graphql_string(&turn.node_did)
    );
    rows_when(&turn.client, "NodeReadiness", &query, |data| {
        data["NodeReadiness"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| {
                let row: NodeReadinessRow = serde_json::from_value(row.clone()).unwrap();
                let snapshot = decode_node_readiness_snapshot(&row, &turn.node_did).unwrap();
                snapshot.process_state.accepts_work()
                    && snapshot.active_generation > generation
                    && snapshot.router_generation == snapshot.active_generation
            })
    })
    .await;
    turn.submit(turn.request("profile-second", session, "second-model"))
        .await
        .unwrap();
    assert_eq!(
        wait_for_request_terminal(&turn.client, "profile-second").await,
        "completed"
    );
    let second = backend.observed_turn_completion_bodies();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["model"], next_model);
    let contexts: Vec<_> = second[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| {
            message["role"] == "user" && message.to_string().contains("Your profile's model:")
        })
        .collect();
    let latest = contexts
        .last()
        .expect("request context names the current model")
        .to_string();
    assert!(
        latest.contains(&format!("Your profile's model: {next_model}.")),
        "{latest}"
    );
    assert!(
        !latest.contains(&format!("Your profile's model: {MODEL}.")),
        "{latest}"
    );
    let system = |body: &Value| {
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        system(&first[0]),
        system(&second[0]),
        "profile changes preserve the stable preamble"
    );
    assert!(!system(&second[0])
        .iter()
        .any(|message| message.to_string().contains("Your profile's model:")));
    runtime.shutdown().await;
}
