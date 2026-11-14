use std::time::Duration;

use gents_documents::default_inference_profile_id_for_agent;
use gents_documents::{AgentContext, CompactionConfig, InferenceSampling};
use gents_inference::compaction::CompactionStrategy;
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;

use crate::support::agent_requests::RequestActors;
use crate::support::builder_runtime::{seed_session_message, submit_request};
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::test_db;

const PROFILE_SEED: i64 = 424_242;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seeded_profile_completes_requests_and_compacts_retained_history() {
    let target = live_target().await;

    let db = test_db("live-seed").await;
    let actors = bind_target(&db, &target).await;
    let (node_did, agent_did) = (actors.node_did().to_owned(), actors.agent_did().to_owned());
    let reader = &actors.fixture.access;

    let profile_id = default_inference_profile_id_for_agent(&agent_did);
    configure_seed_and_compaction(&actors, &node_did, &agent_did, &profile_id).await;
    let sampling_id = escape_graphql_string(&format!("{profile_id}:sampling"));
    let sampling = reader
        .execute(&format!(
            "{{ InferenceSampling(filter: {{sampling_id: {{_eq: \"{sampling_id}\"}}}}) {{seed}} }}"
        ))
        .await
        .unwrap();
    assert_eq!(
        sampling["data"]["InferenceSampling"][0]["seed"].as_i64(),
        Some(PROFILE_SEED)
    );

    let profile_request_id = "req-live-profile-seed";
    submit_request(
        &actors,
        profile_request_id,
        "session-live-profile-seed",
        "interactive",
        "Reply with the single lowercase word: profile",
    )
    .await;

    let second_request_id = "req-live-second-profile-seed";
    submit_request(
        &actors,
        second_request_id,
        "session-live-second-profile-seed",
        "interactive",
        "Reply with the single lowercase word: second",
    )
    .await;

    let compaction_request_id = "req-live-compaction-seed";
    let compaction_session_id = "session-live-compaction-seed";
    submit_request(
        &actors,
        compaction_request_id,
        compaction_session_id,
        "interactive",
        "Use the retained context and reply with the single lowercase word: compacted",
    )
    .await;
    seed_compaction_history(&actors, compaction_session_id).await;

    let agent = boot_live_agent(&db, db.node_identity.clone())
        .await
        .expect("boot live agent");

    for request_id in [profile_request_id, second_request_id, compaction_request_id] {
        let terminal =
            wait_for_request_terminal(reader, request_id, Duration::from_secs(120)).await;
        assert_eq!(
            terminal, "completed",
            "target {} must accept and complete seeded request {request_id}",
            target.name
        );

        let answer = wait_for_assistant_answer(reader, request_id, Duration::from_secs(30)).await;
        assert!(
            !answer.trim().is_empty(),
            "seeded request {request_id} must persist a non-empty response"
        );
    }
    let compacted = reader.execute(r#"{ CompactionEntry(filter: {session_id: {_eq: "session-live-compaction-seed"}}) {request_id summary compacted_through_sequence} }"#).await.unwrap();
    assert!(compacted["data"]["CompactionEntry"].as_array().unwrap().iter().any(|row|
        row["request_id"] == compaction_request_id && row["summary"].as_str().is_some_and(|summary| !summary.trim().is_empty())
        && row["compacted_through_sequence"].as_i64().is_some_and(|sequence| sequence > 0)),
        "the seeded profile must complete after persisting a usable compaction of its retained history: {compacted}");

    agent.shutdown().await;
}

async fn configure_seed_and_compaction(
    actors: &RequestActors,
    node_did: &str,
    agent_did: &str,
    profile_id: &str,
) {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{
        read_desired_state_record_in_txn as read, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    actors
        .fixture
        .access
        .transact_write("test.configure_live_seed", |txn| {
            Box::pin(async move {
                let (_, profile) = read(txn, Collection::InferenceProfile, node_did, profile_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("default inference profile is missing"))?;
                let mut profile: gents_documents::InferenceProfile =
                    serde_json::from_value(profile)?;
                let mut sampling = match profile.sampling_id.as_deref() {
                    Some(id) => {
                        let (_, value) = read(txn, Collection::InferenceSampling, node_did, id)
                            .await?
                            .ok_or_else(|| {
                                anyhow::anyhow!("selected inference sampling is missing")
                            })?;
                        serde_json::from_value::<InferenceSampling>(value)?
                    }
                    None => InferenceSampling::default(),
                };
                let (_, behavior) = read(txn, Collection::Agent, node_did, agent_did)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("default behavior is missing"))?;
                let mut behavior: gents_documents::Agent = serde_json::from_value(behavior)?;
                let context_id = behavior
                    .context_id
                    .clone()
                    .unwrap_or_else(|| format!("{agent_did}:context"));
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
                let compaction_id = format!("{agent_did}:compaction");
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
                apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .expect("configure live seed and compaction");
}

async fn seed_compaction_history(actors: &RequestActors, session_id: &str) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    for turn in 0..10 {
        let sequence = turn * 2 + 1;
        seed_session_message(
            actors,
            session_id,
            sequence,
            "user",
            &format!("retained context turn {turn}"),
            &timestamp,
        )
        .await;
        seed_session_message(
            actors,
            session_id,
            sequence + 1,
            "assistant",
            &format!("retained answer {turn}: {}", "x".repeat(10_000)),
            &timestamp,
        )
        .await;
    }
}
