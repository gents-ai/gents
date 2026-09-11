//! LIVE qualification for sampling-seed propagation through the complete
//! request path and a real OpenAI-compatible provider.
//!
//! The deterministic tests retain the schema, hydration, precedence, and exact
//! body-shape contracts. This test establishes the provider-facing behavior
//! they cannot: the configured live provider accepts the request produced by
//! Gents, the request completes, and the pre-send durable capture contains the
//! effective profile seed for ordinary inference and a model-backed compaction
//! continuation.
//!
//! ```bash
//! GENTS_D4F_LIVE=1 cargo test -p gents --test e2e_live \
//!   d4f_live_seeds_reach_the_provider \
//!   -- --ignored --test-threads=1 --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::document_config::{AgentContext, CompactionConfig, InferenceSampling};
use gents::graphql::escape_graphql_string;
use gents::{
    default_inference_profile_id_for_behavior, AgentIdentity, Collection, CompactionStrategy,
};
use serde::Deserialize;

use crate::steward_loop_live::{
    bind_d4f_backend, boot_d4f_agent, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::fixtures::test_identity;
use crate::support::interrupt::create_runtime_request;
use crate::support::{create_agent_message, test_db};

const PROFILE_SEED: i64 = 424_242;

#[derive(Debug, Deserialize)]
struct RenderedRequestRow {
    capture_scope: String,
    source: String,
    request_json: String,
}

fn d4f_enabled() -> bool {
    std::env::var("GENTS_D4F_LIVE").as_deref() == Ok("1")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_D4F_LIVE=1 and pass --ignored"]
async fn d4f_live_seeds_reach_the_provider() {
    assert!(
        d4f_enabled(),
        "set GENTS_D4F_LIVE=1 and pass --ignored to run the live seed qualification"
    );

    let db = test_db("d4f-live-seed").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("d4f-live-seed"));
    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;

    let profile_id = default_inference_profile_id_for_behavior(&behavior_id);
    configure_seed_and_compaction(db.node.as_ref(), &agent_did, &behavior_id, &profile_id).await;

    // Create requests before boot so every provider call resolves the same
    // profile-owned sampling document before the daemon can claim them.
    let profile_request_id = "req-d4f-profile-seed";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        profile_request_id,
        "session-d4f-profile-seed",
        "Reply with the single lowercase word: profile",
    )
    .await;

    let second_request_id = "req-d4f-second-profile-seed";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        second_request_id,
        "session-d4f-second-profile-seed",
        "Reply with the single lowercase word: second",
    )
    .await;

    let compaction_request_id = "req-d4f-compaction-seed";
    let compaction_session_id = "session-d4f-compaction-seed";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        compaction_request_id,
        compaction_session_id,
        "Use the retained context and reply with the single lowercase word: compacted",
    )
    .await;
    seed_compaction_history(db.node.as_ref(), compaction_session_id).await;

    let agent = boot_d4f_agent(&db, identity).await.expect("boot d4f agent");

    for (request_id, expected_seed, expect_compaction) in [
        (profile_request_id, PROFILE_SEED, false),
        (second_request_id, PROFILE_SEED, false),
        (compaction_request_id, PROFILE_SEED, true),
    ] {
        let terminal =
            wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(120)).await;
        assert_eq!(
            terminal, "completed",
            "d4f must accept and complete seeded request {request_id}"
        );

        let answer =
            wait_for_assistant_answer(db.node.as_ref(), request_id, Duration::from_secs(30)).await;
        assert!(
            !answer.trim().is_empty(),
            "seeded d4f request {request_id} must persist a non-empty response"
        );

        let rows = rendered_requests(db.node.as_ref(), request_id).await;
        let inference_rows = rows
            .iter()
            .filter(|row| row.capture_scope.starts_with("inference."))
            .collect::<Vec<_>>();
        assert!(
            !inference_rows.is_empty(),
            "seeded request {request_id} must retain its exact provider body"
        );
        let compaction_rows = rows
            .iter()
            .filter(|row| row.capture_scope.starts_with("compaction."))
            .collect::<Vec<_>>();
        assert_eq!(
            !compaction_rows.is_empty(),
            expect_compaction,
            "request {request_id} compaction expectation must match its captured provider calls"
        );
        for row in inference_rows.into_iter().chain(compaction_rows) {
            assert_eq!(row.source, "openai_chat_completions");
            let body: serde_json::Value =
                serde_json::from_str(&row.request_json).expect("captured request is JSON");
            assert_eq!(
                body.get("seed").and_then(serde_json::Value::as_i64),
                Some(expected_seed),
                "every live inference attempt must retain the effective seed"
            );
        }
    }

    agent.shutdown().await;
}

async fn configure_seed_and_compaction(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    profile_id: &str,
) {
    use gents::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn as read,
        DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    gents::ConfigAccess::transact_local(node, None, "test.configure_live_seed", |txn| {
        Box::pin(async move {
            let (_, profile) = read(txn, Collection::InferenceProfile, agent_did, profile_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("default inference profile is missing"))?;
            let mut profile: gents::InferenceProfile = serde_json::from_value(profile)?;
            let (_, behavior) = read(txn, Collection::AgentBehavior, agent_did, behavior_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("default behavior is missing"))?;
            let mut behavior: gents::document_config::AgentBehavior =
                serde_json::from_value(behavior)?;
            let context_id = behavior
                .context_id
                .clone()
                .unwrap_or_else(|| format!("{behavior_id}:context"));
            let mut context = read(txn, Collection::AgentContext, agent_did, &context_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<AgentContext>(value))
                .transpose()?
                .unwrap_or_else(|| AgentContext {
                    context_id: context_id.clone(),
                    agent_did: agent_did.to_string(),
                    display_name: None,
                    description: None,
                    system_prompt: None,
                    tools_id: None,
                    compaction_id: None,
                    skill_ids: Vec::new(),
                    tags: Vec::new(),
                });
            let sampling_id = format!("{profile_id}:sampling");
            let compaction_id = format!("{behavior_id}:compaction");
            profile.sampling_id = Some(sampling_id.clone());
            profile.context_window = Some(64_000);
            profile.max_output_tokens = Some(512);
            behavior.context_id = Some(context_id);
            context.compaction_id = Some(compaction_id.clone());
            let sampling = InferenceSampling {
                agent_did: agent_did.to_string(),
                sampling_id,
                seed: Some(PROFILE_SEED),
                ..Default::default()
            };
            let compaction = CompactionConfig {
                compaction_id,
                agent_did: agent_did.to_string(),
                display_name: None,
                strategy: CompactionStrategy::StripThenSummarize,
                threshold: Some(0.25),
                keep_recent_tokens: None,
                tool_result_max_chars: None,
                summary_max_output_tokens: None,
                summary_file_list_max: None,
                inference_profile_id: None,
                tags: Vec::new(),
            };
            let documents = [
                (
                    Collection::InferenceSampling,
                    serde_json::to_value(sampling)?,
                ),
                (Collection::Compaction, serde_json::to_value(compaction)?),
                (Collection::InferenceProfile, serde_json::to_value(profile)?),
                (Collection::AgentContext, serde_json::to_value(context)?),
                (Collection::AgentBehavior, serde_json::to_value(behavior)?),
            ];
            let plan = DesiredStateApplyPlan::new(
                documents
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
            )?;
            apply_desired_state_plan(txn, &plan).await.map(|_| ())
        })
    })
    .await
    .expect("configure live seed and compaction");
}

async fn seed_compaction_history(node: &EmbeddedNode, session_id: &str) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    for turn in 0..10 {
        let sequence = turn * 2 + 1;
        create_agent_message(
            node,
            session_id,
            sequence,
            "user",
            &format!("retained context turn {turn}"),
            &timestamp,
        )
        .await;
        create_agent_message(
            node,
            session_id,
            sequence + 1,
            "assistant",
            &format!("retained answer {turn}: {}", "x".repeat(10_000)),
            &timestamp,
        )
        .await;
    }
}

async fn rendered_requests(node: &EmbeddedNode, request_id: &str) -> Vec<RenderedRequestRow> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            RenderedRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                capture_scope
                source
                request_json
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "RenderedRequest query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("RenderedRequest"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .expect("decode RenderedRequest rows")
        .unwrap_or_default()
}
