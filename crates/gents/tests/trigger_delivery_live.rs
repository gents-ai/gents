#![cfg(feature = "live-e2e")]

mod support;

use anyhow::{ensure, Context, Result};
use gents::documents::apply_desired_state_plan;
use gents::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::store::graphql::escape_graphql_string;
use gents::{Collection, ConfigAccess, NodeIdentity};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use support::live_inference::{boot_live_agent, terminal_assistant_answer};

fn artifact_directory(label: &str) -> Result<std::path::PathBuf> {
    let root = std::env::var_os("GENTS_DELIVERY_ARTIFACT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root)?;
    let source_sha = std::env::var("GENTS_DELIVERY_SOURCE_SHA")
        .context("GENTS_DELIVERY_SOURCE_SHA must identify the committed source under test")?;
    ensure!(
        source_sha.len() == 40 && source_sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "GENTS_DELIVERY_SOURCE_SHA must be a full commit SHA"
    );
    let directory = tempfile::Builder::new()
        .prefix(&format!("gents-delivery-{label}-"))
        .tempdir_in(root)?
        .keep();
    std::fs::write(
        directory.join("source-commit.txt"),
        format!("{source_sha}\n"),
    )?;
    Ok(directory)
}

fn provider_peaks(calls: &[Value]) -> Result<BTreeMap<String, usize>> {
    let mut events: BTreeMap<String, Vec<(chrono::DateTime<chrono::FixedOffset>, i32)>> =
        BTreeMap::new();
    for call in calls.iter().filter(|row| row["call_kind"] == "inference") {
        let start = chrono::DateTime::parse_from_rfc3339(string(call, "started_at")?)?;
        let end = chrono::DateTime::parse_from_rfc3339(string(call, "ended_at")?)?;
        ensure!(end > start, "invalid provider interval: {call}");
        for key in [string(call, "backend_id")?, "all"] {
            events
                .entry(key.into())
                .or_default()
                .extend([(start, 1), (end, -1)]);
        }
    }
    let mut peaks = BTreeMap::new();
    for (backend, mut edges) in events {
        edges.sort();
        let (mut active, mut peak) = (0i32, 0i32);
        for (_, delta) in edges {
            active += delta;
            peak = peak.max(active);
        }
        ensure!(active == 0, "provider interval imbalance");
        let bound = if backend == "all" { 64 } else { 32 };
        ensure!(
            peak <= bound,
            "backend admission exceeded configured capacity: {backend} peak={peak}"
        );
        peaks.insert(backend, peak as usize);
    }
    Ok(peaks)
}

const RUN_START_SDL: &str = r#"
type DeliveryRunStart {
    handoff_id: String @immutable
    tags: [String!]
}
"#;
const WORK_SDL: &str = r#"
type DeliveryWork {
    handoff_id: String @immutable
    tags: [String!]
    reply_session_id: String @immutable
    lane: Int @immutable
    shard_id: String @immutable
    attempt: Int @immutable
}
"#;

/// Installs one application collection as the household-owner User, the
/// actor whose key signs the qualification's source documents.
async fn install_schema(access: &ConfigAccess, sdl: &str) -> Result<()> {
    let plan = gents::store::config_client::preview_schema_install(access, sdl).await?;
    gents::store::config_client::apply_schema_install(access, sdl, &plan.artifact_digest).await?;
    Ok(())
}

/// Creates one User-signed source document for an event trigger.
async fn create_source(
    access: &ConfigAccess,
    collection: &str,
    sdl: &str,
    fields: Value,
) -> Result<String> {
    Ok(gents::store::config_client::create_authored_document(
        access,
        collection,
        &fields,
        gents::store::config_client::DocumentSchemaSource::UserAuthored,
        sdl,
    )
    .await?)
}

async fn apply(
    access: &ConfigAccess,
    owner: &str,
    documents: Vec<(Collection, Value)>,
) -> Result<()> {
    let documents = documents
        .into_iter()
        .map(|(collection, mut value)| {
            value["node_did"] = json!(owner);
            value["tags"] = json!(["test", "trigger-delivery-live"]);
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect();
    let plan = DesiredStateApplyPlan::new(documents)?;
    access
        .transact_write("test.delivery_live.configure", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
}

async fn rows(
    access: &ConfigAccess,
    collection: &str,
    fields: &str,
    owner: &str,
) -> Result<Vec<Value>> {
    let owner_field = if matches!(collection, "TriggerFire" | "FireOutcome") {
        "owner_did"
    } else {
        "node_did"
    };
    let owner = escape_graphql_string(owner);
    let result = access.execute(&format!(
        "{{ {collection}(filter: {{ {owner_field}: {{ _eq: \"{owner}\" }} }}, limit: 1000) {{ {fields} }} }}"
    )).await?;
    Ok(result["data"][collection]
        .as_array()
        .context("query rows")?
        .clone())
}

fn string<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row[field]
        .as_str()
        .with_context(|| format!("missing {field} in {row}"))
}

/// The qualification's provisioned Agents, identified by their derived DIDs.
struct DeliveryAgents {
    lead: String,
    workers: [String; 2],
}

/// The fixture User's held access to the qualification's retained home.
async fn user_access(db: &support::TestDb) -> ConfigAccess {
    let user = support::load_managed_fixture_user(db).await;
    db.client_access(&user)
}

/// Points the Node's default at the lead and each Agent at its lane profile.
async fn bind_agents(access: &ConfigAccess, owner: &str, agents: &DeliveryAgents) -> Result<()> {
    let profiles = [
        (agents.lead.clone(), "delivery-profile-0".to_owned()),
        (agents.workers[0].clone(), "delivery-profile-0".to_owned()),
        (agents.workers[1].clone(), "delivery-profile-1".to_owned()),
    ];
    access
        .transact_write("test.delivery_live.bind_agents", |txn| {
            use gents::store::config_client::read_desired_state_record_in_txn as read;
            let profiles = &profiles;
            let lead = &agents.lead;
            Box::pin(async move {
                let (_, node) = read(txn, Collection::Node, owner, owner)
                    .await?
                    .context("the fixture User's Node is missing")?;
                let mut node: gents::documents::Node = serde_json::from_value(node)?;
                node.default_agent_did = Some(lead.clone());
                let mut documents = vec![(Collection::Node, serde_json::to_value(node)?)];
                for (agent_did, profile_id) in profiles {
                    let (_, agent) = read(txn, Collection::Agent, owner, agent_did)
                        .await?
                        .with_context(|| format!("provisioned Agent {agent_did} is missing"))?;
                    let mut agent: gents::documents::Agent = serde_json::from_value(agent)?;
                    agent.inference_profile_id = profile_id.clone();
                    documents.push((Collection::Agent, serde_json::to_value(agent)?));
                }
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
}

/// Provisions the lead and two worker Agents as the household-owner User and
/// configures their lanes, tasks and triggers under that User's signature.
async fn configure(
    db: &support::TestDb,
    endpoints: &[String; 2],
) -> Result<(support::ManagedAgentFixture, DeliveryAgents)> {
    let fixture = support::managed_agent_fixture(db).await;
    support::grant_household_owner(db, &fixture.user).await;
    let owner = db.node_identity.did().to_owned();
    let mut workers = Vec::new();
    for lane in 0..2 {
        let (_, binding) = support::install_managed_agent(
            db,
            &fixture.access,
            &fixture.user,
            &format!("Delivery worker {}", lane + 1),
        )
        .await;
        workers.push(binding.agent_did);
    }
    let agents = DeliveryAgents {
        lead: fixture.binding.agent_did.clone(),
        workers: [workers[0].clone(), workers[1].clone()],
    };
    for sdl in [RUN_START_SDL, WORK_SDL] {
        install_schema(&fixture.access, sdl).await?;
    }
    let mut documents = vec![(
        Collection::InferenceExecution,
        json!({
            "execution_id":"delivery-execution", "max_turns":4,
            "deadline_duration_secs":1800, "stream_liveness_timeout_secs":180
        }),
    )];
    for (lane, endpoint) in endpoints.iter().enumerate() {
        documents.extend([
            (Collection::InferenceBackend, json!({
                "backend_id":format!("delivery-backend-{lane}"), "name":format!("Delivery workstation {}", lane+1),
                "provider_kind":"OpenAiCompatible", "openai_wire_api":"chat_completions",
                "endpoint":endpoint, "auth":{"kind":"unauthenticated"}, "enabled":true,
                "max_concurrent":32, "max_queue_depth":128
            })),
            (Collection::InferenceProfile, json!({
                "profile_id":format!("delivery-profile-{lane}"),
                "backend_id":format!("delivery-backend-{lane}"), "model_name":"GLM-5.3-Flash-NVFP4",
                "context_window":1_000_000, "max_output_tokens":2048,
                "execution_id":"delivery-execution"
            })),
            (Collection::Task, json!({
                "task_id":format!("delivery-worker-{lane}"), "agent_did":&agents.workers[lane],
                "emit_outcome":true,
                "prompt_template":"Reply exactly WORK {{ doc.handoff_id }}. Do not call tools."
            })),
            (Collection::EventSource, json!({
                "event_source_id":format!("delivery-worker-{lane}"), "source_collection":"DeliveryWork",
                "event_kind":"created", "filter":format!("{{ lane: {{ _eq: {lane} }} }}")
            })),
            (Collection::Trigger, json!({
                "trigger_id":format!("delivery-worker-{lane}"), "task_id":format!("delivery-worker-{lane}"),
                "source":{"kind":"event", "event_source_id":format!("delivery-worker-{lane}")},
                "concurrency":"parallel", "enabled":true
            })),
        ]);
    }
    documents.extend([
        (Collection::Task, json!({
            "task_id":"delivery-start", "agent_did":&agents.lead, "emit_outcome":false,
            "prompt_template":"Reply exactly READY {{ session.session_id }} {{ doc.handoff_id }}. Do not call tools."
        })),
        (Collection::Task, json!({
            "task_id":"delivery-inbox", "agent_did":&agents.lead, "emit_outcome":false,
            "prompt_template":"Reply exactly ACK {{ session.session_id }} {{ doc.source_handoff_id }}. Do not call tools."
        })),
        (Collection::EventSource, json!({"event_source_id":"delivery-start", "source_collection":"DeliveryRunStart", "event_kind":"created"})),
        (Collection::EventSource, json!({
            "event_source_id":"delivery-inbox", "source_collection":"FireOutcome", "event_kind":"created",
            "filter":format!("{{ owner_did: {{ _eq: \"{}\" }}, source_collection: {{ _eq: \"DeliveryWork\" }} }}", escape_graphql_string(&owner))
        })),
        (Collection::Trigger, json!({
            "trigger_id":"delivery-start", "task_id":"delivery-start", "enabled":true,
            "source":{"kind":"event", "event_source_id":"delivery-start"}, "concurrency":"parallel"
        })),
        (Collection::Trigger, json!({
            "trigger_id":"delivery-inbox", "task_id":"delivery-inbox", "enabled":true,
            "source":{"kind":"event", "event_source_id":"delivery-inbox"}, "concurrency":"parallel",
            "session_id_template":"{{ doc.reply_session_id }}"
        })),
    ]);
    apply(&fixture.access, &owner, documents).await?;
    bind_agents(&fixture.access, &owner, &agents).await?;
    for agent_did in [&agents.lead, &agents.workers[0], &agents.workers[1]] {
        support::fixtures::configure_agent_tools(
            db, &owner, agent_did,
            Some("Follow the requested exact one-line response. This is a delivery qualification; never call tools.".into()),
            gents::documents::Tools {
                tools_id: format!("{agent_did}:tools"), node_did: owner.clone(), ..Default::default()
            }, Vec::new(),
        ).await;
    }
    Ok((fixture, agents))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "real inference: GENTS_TRIGGER_DELIVERY_LIVE=1; GENTS_DELIVERY_ENDPOINT_1 and GENTS_DELIVERY_ENDPOINT_2"]
async fn two_lead_sessions_route_64_real_worker_outcomes_without_chaining() -> Result<()> {
    ensure!(
        std::env::var("GENTS_TRIGGER_DELIVERY_LIVE").as_deref() == Ok("1"),
        "explicit live opt-in required"
    );
    let endpoints = [
        std::env::var("GENTS_DELIVERY_ENDPOINT_1").context("first workstation /v1 endpoint")?,
        std::env::var("GENTS_DELIVERY_ENDPOINT_2").context("second workstation /v1 endpoint")?,
    ];
    ensure!(
        endpoints[0] != endpoints[1],
        "two distinct inference endpoints required"
    );
    let artifacts = artifact_directory("burst")?;
    let home =
        gents::eval::runner::embedded::EmbeddedHome::create_retained(&artifacts.join("home"))
            .await?;
    let db = support::test_db_from_home(home);
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let owner = identity.did().to_owned();
    let (fixture, agents) = configure(&db, &endpoints).await?;
    let access = fixture.access;
    let runtime = boot_live_agent(&db, identity).await?;
    support::interrupt::wait_for_runtime_ready(&db.node, &owner).await;
    for label in ["lead-a", "lead-b"] {
        create_source(
            &access,
            "DeliveryRunStart",
            RUN_START_SDL,
            json!({"handoff_id": label, "tags": ["test"]}),
        )
        .await?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
    let leads = loop {
        let fires = rows(
            &access,
            "TriggerFire",
            "fire_key trigger_id source_doc_id request_id session_id",
            &owner,
        )
        .await?;
        let leads = fires
            .into_iter()
            .filter(|row| row["trigger_id"] == "delivery-start")
            .collect::<Vec<_>>();
        if leads.len() == 2 {
            break leads;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "lead admission timed out: {leads:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let sessions = leads
        .iter()
        .map(|row| string(row, "session_id").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        sessions[0] != sessions[1],
        "independent lead fires reused a session"
    );
    let mut expected = BTreeMap::new();
    for shard in 0..64 {
        let handoff = format!("assignment-{shard:02}");
        let session = &sessions[(shard / 2) % 2];
        expected.insert(handoff.clone(), session.clone());
        create_source(
            &access,
            "DeliveryWork",
            WORK_SDL,
            json!({
                "handoff_id": handoff, "reply_session_id": session, "lane": shard % 2,
                "shard_id": format!("shard-{shard:02}"), "attempt": 1, "tags": ["test"],
            }),
        )
        .await?;
    }
    let mut observed_busy_queue = false;
    let mut next_evidence = tokio::time::Instant::now();
    let (requests, fires, outcomes) = loop {
        let requests = rows(
            &access,
            "AgentRequest",
            "request_id session_id agent_did content lifecycle_state failure_reason",
            &owner,
        )
        .await?;
        ensure!(
            requests.iter().all(|row| {
                !gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
                    row["lifecycle_state"].as_str(),
                ) || row["lifecycle_state"] == "completed"
            }),
            "live delivery request failed: {requests:?}"
        );
        for session in &sessions {
            let scoped = requests
                .iter()
                .filter(|row| row["session_id"] == session.as_str())
                .collect::<Vec<_>>();
            let running = scoped
                .iter()
                .filter(|row| row["lifecycle_state"] == "processing")
                .count();
            ensure!(
                running <= 1,
                "concurrent requests in one lead session: {scoped:?}"
            );
            observed_busy_queue |=
                running == 1 && scoped.iter().any(|row| row["lifecycle_state"] == "pending");
        }
        let outcomes = rows(&access, "FireOutcome", "handoff_id fire_key trigger_id source_doc_id request_id session_id source_handoff_id reply_session_id terminal_state shard_id attempt", &owner).await?;
        let fires = rows(
            &access,
            "TriggerFire",
            "fire_key trigger_id source_doc_id request_id session_id emit_outcome",
            &owner,
        )
        .await?;
        if tokio::time::Instant::now() >= next_evidence {
            let calls = rows(
                &access,
                "InferenceCall",
                "request_id backend_id call_kind call_state started_at ended_at",
                &owner,
            )
            .await?;
            let triggers = rows(
                &access,
                "Trigger",
                "trigger_id last_status last_error",
                &owner,
            )
            .await?;
            std::fs::write(
                artifacts.join("progress.json"),
                serde_json::to_vec_pretty(&json!({
                    "requests": requests, "fires": fires, "outcomes": outcomes,
                    "calls": calls, "triggers": triggers,
                }))?,
            )?;
            next_evidence = tokio::time::Instant::now() + Duration::from_secs(5);
        }
        if requests.len() == 130
            && outcomes.len() == 64
            && fires.len() == 130
            && requests
                .iter()
                .all(|row| row["lifecycle_state"] == "completed")
        {
            break (requests, fires, outcomes);
        }
        ensure!(tokio::time::Instant::now() < deadline,
            "delivery timed out: {} requests, {} receipts, {} outcomes; requests={requests:?}; outcomes={outcomes:?}",
            requests.len(), fires.len(), outcomes.len());
        ensure!(
            outcomes.len() <= 64,
            "outcome consumer chained: {outcomes:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    ensure!(
        observed_busy_queue,
        "did not observe an inbox queued behind its busy lead; qualification incomplete"
    );
    for (collection, records, key) in [
        ("request", &requests, "request_id"),
        ("fire", &fires, "fire_key"),
        ("outcome", &outcomes, "handoff_id"),
    ] {
        let keys = records
            .iter()
            .map(|row| string(row, key))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(
            keys.len() == records.len(),
            "duplicate {collection} identity"
        );
    }
    let calls = rows(
        &access,
        "InferenceCall",
        "request_id backend_id call_kind call_state started_at ended_at",
        &owner,
    )
    .await?;
    for request in &requests {
        let expected_backend = if request["agent_did"] == agents.workers[1].as_str() {
            "delivery-backend-1"
        } else {
            "delivery-backend-0"
        };
        ensure!(
            calls
                .iter()
                .any(|call| call["request_id"] == request["request_id"]
                    && call["backend_id"] == expected_backend
                    && call["call_kind"] == "inference"
                    && call["call_state"] == "completed"),
            "no completed real inference on {expected_backend} for {request}"
        );
    }
    for (lane, worker) in agents.workers.iter().enumerate() {
        ensure!(
            requests
                .iter()
                .filter(|row| row["agent_did"] == worker.as_str())
                .count()
                == 32,
            "workstation {lane} did not execute 32 assignments"
        );
    }
    for outcome in &outcomes {
        let handoff = string(outcome, "source_handoff_id")?;
        let target = expected
            .remove(handoff)
            .context("duplicate or unknown outcome handoff")?;
        ensure!(
            outcome["reply_session_id"] == target,
            "outcome lost lead routing: {outcome}"
        );
        ensure!(
            outcome["terminal_state"] == "completed",
            "worker failed: {outcome}"
        );
        ensure!(outcome["attempt"] == 1, "outcome lost attempt: {outcome}");
        let inboxes = requests
            .iter()
            .filter(|row| {
                row["agent_did"] == agents.lead.as_str()
                    && row["content"]
                        .as_str()
                        .is_some_and(|text| text.contains(handoff))
            })
            .collect::<Vec<_>>();
        ensure!(
            inboxes.len() == 1,
            "handoff must enter exactly one lead inbox: {inboxes:?}"
        );
        ensure!(
            inboxes[0]["session_id"] == target,
            "handoff reached the other lead"
        );
        let answer = terminal_assistant_answer(&access, string(inboxes[0], "request_id")?).await;
        ensure!(
            answer.contains(&target) && answer.contains(handoff),
            "lead inference did not acknowledge its own rendered session: {answer}"
        );
    }
    ensure!(expected.is_empty(), "missing outcomes: {expected:?}");
    for lead in leads {
        let request = requests
            .iter()
            .find(|row| row["request_id"] == lead["request_id"])
            .context("lead request missing")?;
        ensure!(
            string(request, "content")?.contains(string(&lead, "session_id")?),
            "lead template rendered a different session"
        );
        let answer = terminal_assistant_answer(&access, string(&lead, "request_id")?).await;
        ensure!(
            answer.contains(string(&lead, "session_id")?),
            "lead did not receive its rendered session: {answer}"
        );
    }
    let peaks = provider_peaks(&calls)?;
    ensure!(
        peaks["delivery-backend-0"] > 1 && peaks["delivery-backend-1"] > 1,
        "both endpoints must demonstrate concurrent provider calls: {peaks:?}"
    );
    std::fs::write(
        artifacts.join("evidence.json"),
        serde_json::to_vec_pretty(&json!({
            "owner_did":owner, "lead_agent_did":&agents.lead, "worker_agent_dids":&agents.workers,
            "endpoints":endpoints, "configured_backend_capacity":[32,32],
            "provider_peak_overlap":peaks, "lead_sessions":sessions, "requests":requests,
            "fires":fires, "outcomes":outcomes, "inference_calls":calls,
            "observed_busy_queue":observed_busy_queue
        }))?,
    )?;
    runtime.shutdown().await;
    db.node.shutdown().await?;
    Ok(())
}

#[path = "trigger_delivery_live/process.rs"]
mod process;

#[path = "trigger_delivery_live/goal.rs"]
mod goal;
