//! LIVE qualification for sampling-seed propagation through the complete
//! request path and a real OpenAI-compatible provider.
//!
//! The deterministic tests retain the schema, hydration, precedence, and exact
//! body-shape contracts. This test establishes the provider-facing behavior
//! they cannot: DeepSeek V4 Flash accepts the request produced by Gents, the
//! request completes, and the pre-send durable capture contains the effective
//! seed for profile defaulting, a per-request override, and a model-backed
//! compaction continuation.
//!
//! ```bash
//! GENTS_D4F_LIVE=1 cargo test -p gents --test e2e_live \
//!   d4f_live_seeds_reach_the_provider \
//!   -- --ignored --test-threads=1 --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    default_inference_profile_id_for_behavior, load_agent_behavior, load_inference_profile,
    upsert_agent_behavior, upsert_inference_profile, AgentIdentity,
};
use serde::Deserialize;

use crate::steward_loop_live::{
    bind_d4f_backend, boot_d4f_agent, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::fixtures::test_identity;
use crate::support::interrupt::create_runtime_request;
use crate::support::{create_agent_message, test_db};

const PROFILE_SEED: i64 = 424_242;
const REQUEST_SEED: i64 = 818_181;

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
    let mut profile = load_inference_profile(db.node.as_ref(), &profile_id)
        .await
        .expect("load d4f inference profile")
        .expect("default inference profile exists");
    profile.seed = Some(PROFILE_SEED);
    profile.context_window = Some(64_000);
    profile.max_output_tokens = Some(512);
    upsert_inference_profile(db.node.as_ref(), &profile)
        .await
        .expect("set live profile seed");
    let mut behavior = load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .expect("load d4f behavior")
        .expect("default behavior exists");
    behavior.compaction_strategy = Some("Summarize".to_string());
    behavior.compaction_threshold = Some(0.25);
    upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .expect("configure live compaction");

    // Create the requests before boot so setting an override cannot race the
    // daemon's claim. The first inherits the profile seed; the others replace it.
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

    let override_request_id = "req-d4f-request-seed";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        override_request_id,
        "session-d4f-request-seed",
        "Reply with the single lowercase word: override",
    )
    .await;
    set_request_seed(db.node.as_ref(), override_request_id, REQUEST_SEED).await;

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
    set_request_seed(db.node.as_ref(), compaction_request_id, REQUEST_SEED).await;
    seed_compaction_history(db.node.as_ref(), compaction_session_id).await;

    let agent = boot_d4f_agent(&db, identity).await.expect("boot d4f agent");

    for (request_id, expected_seed, expect_compaction) in [
        (profile_request_id, PROFILE_SEED, false),
        (override_request_id, REQUEST_SEED, false),
        (compaction_request_id, REQUEST_SEED, true),
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

async fn set_request_seed(node: &EmbeddedNode, request_id: &str, seed: i64) {
    let request_id = escape_graphql_string(request_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ seed: {seed} }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set request seed failed: {:?}",
        response.errors
    );
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
