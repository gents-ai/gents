use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::documents::{AgentContext, CompactionConfig, InferenceSampling};
use gents::store::graphql::escape_graphql_string;
use gents::{default_inference_profile_id_for_agent, Collection, CompactionStrategy, NodeIdentity};
use serde::Deserialize;

use crate::support::interrupt::{create_runtime_request, ensure_user_titled_session};
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::live_receipt::{
    export_live_receipts, required_artifact_dir, ExpectedCall, ReceiptLimits, ReceiptRequest,
};
use crate::support::{create_agent_message_in_scope, test_db};

const PROFILE_SEED: i64 = 424_242;

#[derive(Debug, Deserialize)]
struct RenderedRequestRow {
    capture_scope: String,
    capture_version: u32,
    source: String,
    request_json: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_EVAL_TARGET and pass --ignored"]
async fn live_seeds_reach_the_provider() {
    let artifact_dir = required_artifact_dir().expect("private live artifact directory");
    let target = live_target();

    let db = test_db("live-seed").await;
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    assert_eq!(db.node.node_identity_did(), Some(identity.did()));
    let (node_did, agent_id) = bind_target(&db.node, identity.as_ref(), &target).await;
    let mut authored_profile = target.profile(&node_did);
    authored_profile.max_output_tokens = Some(
        authored_profile
            .max_output_tokens
            .map_or(512, |cap| cap.min(512)),
    );
    let mut authored_sampling = target
        .sampling(&node_did)
        .expect("live seed receipts require referenced canonical sampling");
    authored_sampling.seed = Some(PROFILE_SEED);
    let expected_call = ExpectedCall::openai_chat(
        &target.backend(&node_did),
        &authored_profile,
        &authored_sampling,
    )
    .expect("bounded authored seed target");

    let profile_id = default_inference_profile_id_for_agent(&agent_id);
    configure_seed_and_compaction(&db.node, &node_did, &agent_id, &profile_id).await;

    // Each session is titled before its first request so the runtime does not
    // spawn auxiliary title inference, whose `title-audit` request would be a
    // sibling the receipt's session request set must also list.
    let profile_request_id = "req-live-profile-seed";
    let profile_session_id = "session-live-profile-seed";
    ensure_user_titled_session(
        &db.node,
        &node_did,
        &agent_id,
        profile_session_id,
        "Live profile seed",
    )
    .await;
    let profile_request_doc_id = create_runtime_request(
        &db.node,
        &node_did,
        &agent_id,
        profile_request_id,
        profile_session_id,
        "Reply with the single lowercase word: profile",
    )
    .await;

    let second_request_id = "req-live-second-profile-seed";
    let second_session_id = "session-live-second-profile-seed";
    ensure_user_titled_session(
        &db.node,
        &node_did,
        &agent_id,
        second_session_id,
        "Live second profile seed",
    )
    .await;
    let second_request_doc_id = create_runtime_request(
        &db.node,
        &node_did,
        &agent_id,
        second_request_id,
        second_session_id,
        "Reply with the single lowercase word: second",
    )
    .await;

    let compaction_request_id = "req-live-compaction-seed";
    let compaction_session_id = "session-live-compaction-seed";
    ensure_user_titled_session(
        &db.node,
        &node_did,
        &agent_id,
        compaction_session_id,
        "Live compaction seed",
    )
    .await;
    let compaction_request_doc_id = create_runtime_request(
        &db.node,
        &node_did,
        &agent_id,
        compaction_request_id,
        compaction_session_id,
        "Use the retained context and reply with the single lowercase word: compacted",
    )
    .await;
    seed_compaction_history(&db.node, &node_did, compaction_session_id).await;

    let agent = boot_live_agent(&db, identity)
        .await
        .expect("boot live agent");
    assert_eq!(agent.node_did, node_did);

    for (request_id, expected_seed, expect_compaction) in [
        (profile_request_id, PROFILE_SEED, false),
        (second_request_id, PROFILE_SEED, false),
        (compaction_request_id, PROFILE_SEED, true),
    ] {
        let terminal =
            wait_for_request_terminal(&db.node, request_id, Duration::from_secs(120)).await;
        assert_eq!(
            terminal, "completed",
            "target {} must accept and complete seeded request {request_id}",
            target.name
        );

        let answer = wait_for_assistant_answer(&db.node, request_id, Duration::from_secs(30)).await;
        assert!(
            !answer.trim().is_empty(),
            "seeded request {request_id} must persist a non-empty response"
        );

        let rows = rendered_requests(&db.node, request_id).await;
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
            let body = gents::inference::rendered_request::decode_capture_json_embedded(
                &db.node,
                row.capture_version,
                &row.request_json,
                gents::inference::rendered_request::CapturePayloadKind::RequestBody,
            )
            .await
            .expect("decode captured provider request");
            assert_eq!(
                body.get("seed").and_then(serde_json::Value::as_i64),
                Some(expected_seed),
                "every live inference attempt must retain the effective seed"
            );
        }
    }

    let receipts = [
        (
            profile_request_id,
            profile_session_id,
            profile_request_doc_id,
            false,
        ),
        (
            second_request_id,
            second_session_id,
            second_request_doc_id,
            false,
        ),
        (
            compaction_request_id,
            compaction_session_id,
            compaction_request_doc_id,
            true,
        ),
    ]
    .into_iter()
    .map(
        |(request_id, session_id, request_doc_id, require_summary)| ReceiptRequest {
            node_did: node_did.clone(),
            requester_did: node_did.clone(),
            agent_id: agent_id.clone(),
            session_id: session_id.to_owned(),
            request_id: request_id.to_owned(),
            request_doc_id: Some(request_doc_id),
            lifecycle_state: "completed".to_owned(),
            ordinary: expected_call.clone(),
            summary: require_summary.then(|| expected_call.clone()),
            require_summary,
        },
    )
    .collect::<Vec<_>>();
    export_live_receipts(
        &gents::ConfigAccess::local(db.node.clone()),
        &artifact_dir,
        "seed-history-compaction",
        &receipts,
        ReceiptLimits::default(),
    )
    .await
    .expect("durable seed, inference and compaction receipts");
    agent.shutdown().await;
}

async fn configure_seed_and_compaction(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    profile_id: &str,
) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn as read,
        DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    gents::ConfigAccess::transact_local_write(node, None, "test.configure_live_seed", |txn| {
        Box::pin(async move {
            let (_, profile) = read(txn, Collection::InferenceProfile, node_did, profile_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("default inference profile is missing"))?;
            let mut profile: gents::InferenceProfile = serde_json::from_value(profile)?;
            let mut sampling = match profile.sampling_id.as_deref() {
                Some(id) => {
                    let (_, value) = read(txn, Collection::InferenceSampling, node_did, id)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("selected inference sampling is missing"))?;
                    serde_json::from_value::<InferenceSampling>(value)?
                }
                None => InferenceSampling::default(),
            };
            let (_, behavior) = read(txn, Collection::Agent, node_did, agent_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("default behavior is missing"))?;
            let mut behavior: gents::documents::Agent = serde_json::from_value(behavior)?;
            let context_id = behavior
                .context_id
                .clone()
                .unwrap_or_else(|| format!("{agent_id}:context"));
            let mut context = read(txn, Collection::AgentContext, node_did, &context_id)
                .await?
                .map(|(_, value)| serde_json::from_value::<AgentContext>(value))
                .transpose()?
                .unwrap_or_else(|| AgentContext {
                    context_id: context_id.clone(),
                    node_did: node_did.to_string(),
                    display_name: None,
                    description: None,
                    system_prompt: None,
                    tools_id: None,
                    compaction_id: None,
                    skill_ids: Vec::new(),
                    tags: Vec::new(),
                });
            let sampling_id = format!("{profile_id}:sampling");
            let compaction_id = format!("{agent_id}:compaction");
            profile.sampling_id = Some(sampling_id.clone());
            profile.context_window = Some(64_000);
            profile.max_output_tokens =
                Some(profile.max_output_tokens.map_or(512, |cap| cap.min(512)));
            behavior.context_id = Some(context_id);
            context.compaction_id = Some(compaction_id.clone());
            sampling.node_did = node_did.to_string();
            sampling.sampling_id = sampling_id;
            sampling.seed = Some(PROFILE_SEED);
            let compaction = CompactionConfig {
                compaction_id,
                node_did: node_did.to_string(),
                display_name: None,
                strategy: CompactionStrategy::StripThenSummarize,
                threshold: Some(0.25),
                keep_recent_tokens: None,
                tool_result_max_chars: None,
                summary_max_output_tokens: profile.max_output_tokens,
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
                (Collection::Agent, serde_json::to_value(behavior)?),
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

async fn seed_compaction_history(node: &EmbeddedNode, node_did: &str, session_id: &str) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    for turn in 0..10 {
        let sequence = turn * 2 + 1;
        create_agent_message_in_scope(
            node,
            node_did,
            Some(node_did),
            session_id,
            sequence,
            "user",
            &format!("retained context turn {turn}"),
            &timestamp,
        )
        .await;
        create_agent_message_in_scope(
            node,
            node_did,
            Some(node_did),
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
                capture_version
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

#[tokio::test]
async fn live_receipt_seed_controls_keep_authored_sampling_and_bound_both_outputs() {
    use crate::support::live_inference::InferenceTarget;
    use gents::store::config_client::read_desired_state_record_in_txn as read;

    for authored_cap in [128, 4096] {
        let target = InferenceTarget::decode(
            "seed-controls".to_owned(),
            serde_json::json!({
                "node": {},
                "inference_backends": [{
                    "backend_id": "b", "name": "test", "provider_kind": "OpenAiCompatible",
                    "endpoint": "http://127.0.0.1:9/v1", "max_concurrent": 1,
                    "auth": {"kind": "unauthenticated"}
                }],
                "inference_profiles": [{
                    "profile_id": "p", "backend_id": "b", "model_name": "test-model",
                    "sampling_id": "s", "max_output_tokens": authored_cap
                }],
                "inference_sampling": [{"sampling_id": "s", "temperature": 1.0, "top_k": 1}]
            }),
        )
        .unwrap();
        let db = test_db("seed-controls").await;
        let identity = db.node_identity.clone();
        assert_eq!(db.node.node_identity_did(), Some(identity.did()));
        let (node_did, agent_id) = bind_target(&db.node, identity.as_ref(), &target).await;
        let profile_id = default_inference_profile_id_for_agent(&agent_id);
        configure_seed_and_compaction(&db.node, &node_did, &agent_id, &profile_id).await;
        let (profile, sampling, compaction) = gents::ConfigAccess::transact_local_readonly(
            &db.node,
            None,
            "test.seed_receipt_controls",
            |txn| {
                let node_did = &node_did;
                let profile_id = &profile_id;
                let agent_id = &agent_id;
                Box::pin(async move {
                    let (_, profile) =
                        read(txn, Collection::InferenceProfile, node_did, profile_id)
                            .await?
                            .unwrap();
                    let sampling_id = profile["sampling_id"].as_str().unwrap();
                    let (_, sampling) =
                        read(txn, Collection::InferenceSampling, node_did, sampling_id)
                            .await?
                            .unwrap();
                    let (_, compaction) = read(
                        txn,
                        Collection::Compaction,
                        node_did,
                        &format!("{agent_id}:compaction"),
                    )
                    .await?
                    .unwrap();
                    Ok::<_, anyhow::Error>((profile, sampling, compaction))
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(profile["max_output_tokens"], authored_cap.min(512));
        assert_eq!(
            compaction["summary_max_output_tokens"],
            authored_cap.min(512)
        );
        assert_eq!(sampling["temperature"], 1.0);
        assert_eq!(sampling["top_k"], 1);
        assert_eq!(sampling["seed"], PROFILE_SEED);
        assert!(sampling["top_p"].is_null());
        assert_eq!(sampling["node_did"], node_did);
        let physical = create_runtime_request(
            &db.node,
            &node_did,
            &agent_id,
            "receipt-seed-controls",
            "receipt-seed-session",
            "not executed",
        )
        .await;
        let row = gents::ConfigAccess::local(db.node.clone())
            .execute(&format!(
                "{{ AgentRequest(docID: \"{}\") {{ _docID node_did requester_did session_id }} }}",
                escape_graphql_string(&physical),
            ))
            .await
            .unwrap();
        assert_eq!(row["data"]["AgentRequest"][0]["node_did"], node_did);
        assert_eq!(row["data"]["AgentRequest"][0]["requester_did"], node_did);
        db.node.shutdown().await;
    }
}
