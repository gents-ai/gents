#![cfg(feature = "live-e2e")]

mod support;

use anyhow::{ensure, Context, Result};
use gents_documents::apply_desired_state_plan;
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_store::graphql::escape_graphql_string;
use gents_store::{collection::Collection, config_client::ConfigAccess, identity::NodeIdentity};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use support::live_inference::{
    boot_live_agent, live_target, terminal_assistant_answer, InferenceTarget,
};
use support::snapshots::SnapshotReader;

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

async fn delivery_lanes() -> Result<[InferenceTarget; 2]> {
    let selected = live_target().await;
    let other = if selected.name == "workstation-1" {
        "workstation-2"
    } else {
        "workstation-1"
    };
    let mut other = tokio::task::spawn_blocking(move || InferenceTarget::load(other)).await??;
    other.discover_model().await?;
    Ok([selected, other])
}

struct DeliveryChanges {
    requests: support::snapshots::SnapshotChanges,
    calls: support::snapshots::SnapshotChanges,
    fires: support::snapshots::SnapshotChanges,
    outcomes: support::snapshots::SnapshotChanges,
    goals: support::snapshots::SnapshotChanges,
}

impl DeliveryChanges {
    async fn new(access: &ConfigAccess) -> Self {
        Self {
            requests: access.subscribe_changes("AgentRequest").await,
            calls: access.subscribe_changes("AgentToolCall").await,
            fires: access.subscribe_changes("TriggerFire").await,
            outcomes: access.subscribe_changes("FireOutcome").await,
            goals: access.subscribe_changes("Goal").await,
        }
    }

    async fn recv(&mut self, deadline: tokio::time::Instant) -> Result<()> {
        tokio::time::timeout_at(deadline, async {
            tokio::select! {
                _ = self.requests.recv() => (),
                _ = self.calls.recv() => (),
                _ = self.fires.recv() => (),
                _ = self.outcomes.recv() => (),
                _ = self.goals.recv() => (),
            }
        })
        .await
        .context("delivery state did not change before its deadline")
    }
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
    let plan = gents_store::config_client::preview_schema_install(access, sdl).await?;
    gents_store::config_client::apply_schema_install(access, sdl, &plan.artifact_digest).await?;
    Ok(())
}

/// Creates one User-signed source document for an event trigger.
async fn create_source(
    access: &ConfigAccess,
    collection: &str,
    sdl: &str,
    fields: Value,
) -> Result<String> {
    Ok(gents_store::config_client::create_authored_document(
        access,
        collection,
        &fields,
        gents_store::config_client::DocumentSchemaSource::UserAuthored,
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
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
                .map(|_| ())
            })
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
            use gents_documents::read_desired_state_record_in_txn as read;
            let profiles = &profiles;
            let lead = &agents.lead;
            Box::pin(async move {
                read(txn, Collection::Node, owner, owner)
                    .await?
                    .context("the fixture User's Node is missing")?;
                let mut documents = vec![(
                    Collection::DefaultAgentSelection,
                    serde_json::json!({"node_did": owner, "default_agent_did": lead}),
                )];
                for (agent_did, profile_id) in profiles {
                    let (_, agent) = read(txn, Collection::Agent, owner, agent_did)
                        .await?
                        .with_context(|| format!("provisioned Agent {agent_did} is missing"))?;
                    let mut agent: gents_documents::Agent = serde_json::from_value(agent)?;
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
}

/// Provisions the lead and two worker Agents as the household-owner User and
/// configures their lanes, tasks and triggers under that User's signature.
async fn configure(
    db: &support::TestDb,
    lanes: &[InferenceTarget; 2],
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
    for (lane, target) in lanes.iter().enumerate() {
        let mut backend = target.backend(&owner);
        backend.backend_id = format!("delivery-backend-{lane}");
        let mut profile = target.profile(&owner);
        profile.profile_id = format!("delivery-profile-{lane}");
        profile.backend_id = backend.backend_id.clone();
        profile.execution_id = Some("delivery-execution".into());
        if let Some(sampling) = target.sampling(&owner) {
            documents.push((
                Collection::InferenceSampling,
                serde_json::to_value(sampling)?,
            ));
        }
        documents.extend([
            (Collection::InferenceBackend, serde_json::to_value(backend)?),
            (Collection::InferenceProfile, serde_json::to_value(profile)?),
            (Collection::Task, json!({
                "task_id":format!("delivery-worker-{lane}"), "agent_did":&agents.workers[lane],
                "emit_outcome":true,
                "prompt_template":"Acknowledge work {{ doc.handoff_id }} briefly. Do not call tools."
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
            "prompt_template":"Acknowledge request {{ doc.handoff_id }} in session {{ session.session_id }} briefly. Do not call tools."
        })),
        (Collection::Task, json!({
            "task_id":"delivery-inbox", "agent_did":&agents.lead, "emit_outcome":false,
            "prompt_template":"Acknowledge completed handoff {{ doc.source_handoff_id }} in session {{ session.session_id }} briefly. Do not call tools."
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
            db,
            &owner,
            agent_did,
            Some("Acknowledge the current request briefly. Never call tools.".into()),
            gents_documents::Tools {
                tools_id: format!("{agent_did}:tools"),
                node_did: owner.clone(),
                ..Default::default()
            },
            Vec::new(),
        )
        .await;
    }
    Ok((fixture, agents))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "uses both local targets; set GENTS_LIVE_TARGET and GENTS_DELIVERY_SOURCE_SHA"]
async fn two_lead_sessions_route_64_real_worker_outcomes_without_chaining() -> Result<()> {
    let lanes = delivery_lanes().await?;
    let artifacts = tokio::task::spawn_blocking(|| artifact_directory("burst")).await??;
    let home =
        gents::runtime::embedded::EmbeddedHome::create_retained(&artifacts.join("home")).await?;
    let db = support::test_db_from_home(home);
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let owner = identity.did().to_owned();
    let (fixture, agents) = configure(&db, &lanes).await?;
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
    let mut fire_changes = access.subscribe_changes("TriggerFire").await;
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
        let _ = tokio::time::timeout_at(deadline, fire_changes.recv()).await;
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
    let mut request_changes = access.subscribe_changes("AgentRequest").await;
    let mut outcome_changes = access.subscribe_changes("FireOutcome").await;
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
            tokio::fs::write(
                artifacts.join("progress.json"),
                serde_json::to_vec_pretty(&json!({
                    "requests": requests, "fires": fires, "outcomes": outcomes,
                    "calls": calls, "triggers": triggers,
                }))?,
            )
            .await?;
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
        let _ = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                _ = request_changes.recv() => (),
                _ = fire_changes.recv() => (),
                _ = outcome_changes.recv() => (),
            }
        })
        .await;
    };
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
            !answer.trim().is_empty(),
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
            !answer.trim().is_empty(),
            "lead did not receive its rendered session: {answer}"
        );
    }
    tokio::fs::write(
        artifacts.join("evidence.json"),
        serde_json::to_vec_pretty(&json!({
            "owner_did":owner, "lead_agent_did":&agents.lead, "worker_agent_dids":&agents.workers,
            "lanes":lanes.iter().map(|target| json!({"endpoint":target.endpoint(), "model":target.model()})).collect::<Vec<_>>(),
            "configured_backend_capacity":lanes.iter().map(|target| target.backend(&owner).max_concurrent).collect::<Vec<_>>(),
            "lead_sessions":sessions, "requests":requests,
            "fires":fires, "outcomes":outcomes, "inference_calls":calls
        }))?,
    ).await?;
    runtime.shutdown().await;
    db.node.shutdown().await?;
    Ok(())
}

#[path = "trigger_delivery_live/process.rs"]
mod process;

#[path = "trigger_delivery_live/goal.rs"]
mod goal;
