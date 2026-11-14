use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::Instant;

use gents::{DocumentRuntimeOptions, Gents};
use gents_documents::{
    read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents_pack::pack::archive::{pack_dir, PackArchive};
use gents_pack::pack::{
    install_prepared_document_pack, prepare_document_pack_archive, DriftPolicy, InstalledVia,
    PackAgentBindings,
};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::{collection::Collection, identity::NodeIdentity};
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};

#[cfg(feature = "live-e2e")]
use crate::support::live_inference::{live_target, InferenceTarget};
use crate::support::snapshots::SnapshotReader;
use crate::support::streaming_backend::{MockStreamingBackend, StreamPlan, StreamResponse};
use crate::support::{interrupt::BootedAgent, test_db, TestDb};

#[cfg(feature = "live-e2e")]
const WORK_UNIT: &str = "change-unit:cycle-accepted";
const FAILURE_UNIT: &str = "change-unit:cycle-inbox";
const MODEL: &str = "workspace-cycle-script";

const WRITER: &str = "change-unit-writer";
const REVIEWER: &str = "change-unit-reviewer";
const INTEGRATOR: &str = "change-unit-integrator";
const RECORDER: &str = "change-unit-record";
const REJECTER: &str = "change-unit-rejected";

enum InferenceSource<'a> {
    Scripted(&'a str),
    #[cfg(feature = "live-e2e")]
    Live(&'a InferenceTarget),
}

struct Cycle {
    db: TestDb,
    user: gents_store::identity::KeyIdentity,
    access: ConfigAccess,
    owner: String,
    agents: PackAgentBindings,
}

impl Cycle {
    async fn install(name: &str, inference: InferenceSource<'_>, root: &Path, repo: &Path) -> Self {
        let db = test_db(name).await;
        let owner = db.node_identity.did().to_owned();
        let user = crate::support::managed_fixture_user(&db).await;
        crate::support::grant_household_owner(&db, &user).await;
        let access = db.client_access(&user);
        apply_documents(
            &access,
            "workspace_cycle.node",
            vec![(Collection::Node, json!({"node_did": owner}))],
        )
        .await;
        install_inference(&access, &owner, inference).await;
        let agents = install_pack(&access, &owner, repo).await;
        let cycle = Self {
            db,
            user,
            access,
            owner,
            agents,
        };
        select_default_agent(&cycle.access, &cycle.owner, cycle.agent(WRITER)).await;
        gents_inference::backends::registry::set_backend_probe_status_as(
            &cycle.access,
            &cycle.owner,
            "workspace-cycle-backend",
            gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
        )
        .await
        .unwrap();
        install_workspace_root(&cycle, root).await;
        cycle
    }

    fn agent(&self, slot: &str) -> &str {
        &self
            .agents
            .get(slot)
            .unwrap_or_else(|| panic!("pack slot {slot} is unbound: {:?}", self.agents))
            .agent_did
    }

    fn agents(&self, slots: &[&str]) -> Vec<&str> {
        slots.iter().map(|slot| self.agent(slot)).collect()
    }

    async fn runtime(&self, root: &Path) -> Gents {
        Gents::from_default_agent_documents(
            self.db.node.clone(),
            self.db.node_identity.clone(),
            DocumentRuntimeOptions {
                backend_admission: Some(self.db.home.backend_admission()),
                tool_ceiling: ToolCeiling::readwrite(root),
                ..Default::default()
            },
        )
        .await
        .expect("the installed change-unit pack routes its tasks")
    }
}

async fn boot_cycle(cycle: &Cycle, root: &Path) -> BootedAgent {
    let agent = cycle.runtime(root).await;
    assert!(
        agent.unavailable_agents().is_empty(),
        "unavailable agents: {:?}",
        agent.unavailable_agents()
    );
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(receiver));
    let runtime = BootedAgent::new(shutdown, handle, cycle.owner.clone());
    let query = format!(
        r#"{{ NodeReadiness(filter: {{node_did: {{_eq: "{}"}}}}) {{node_did snapshot_json updated_at}} }}"#,
        escape_graphql_string(&cycle.owner),
    );
    crate::support::live_inference::wait_for_rows(
        &cycle.access,
        "NodeReadiness",
        &query,
        Duration::from_secs(20),
        |data| {
            let row = data["NodeReadiness"]
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| {
                    serde_json::from_value::<gents_protocol::node_readiness::NodeReadinessRow>(
                        row.clone(),
                    )
                    .ok()
                });
            let projection = gents_protocol::node_readiness::project_node_readiness(
                row.as_ref(),
                &cycle.owner,
                cycle
                    .agents
                    .values()
                    .map(|binding| binding.agent_did.as_str()),
                Some(cycle.agent(WRITER)),
            );
            projection.unknown_reason.is_none()
                && projection.agents.values().all(|state| {
                    matches!(
                        state,
                        gents_protocol::node_readiness::ProjectedAgentReadiness::Ready
                    )
                })
        },
    )
    .await;
    runtime
}

async fn repository_fixture() -> (tempfile::TempDir, std::path::PathBuf, String) {
    tokio::task::spawn_blocking(|| {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("source-repo");
        let (base_sha, _) = create_repository(&repo);
        (root, repo, base_sha)
    })
    .await
    .unwrap()
}

#[cfg(feature = "live-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn change_unit_pack_completes_writer_seal_review_and_host_integration() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "gents=info,gents_migration=warn",
        ))
        .try_init();
    let _p2p = super::P2P_E2E_LOCK.lock().await;
    tokio::task::spawn_blocking(ensure_native_fs_runner_for_test)
        .await
        .unwrap();
    let target = live_target().await;
    let (root, repo, base_sha) = repository_fixture().await;
    let cycle = Cycle::install(
        "workspace-change-unit-cycle",
        InferenceSource::Live(&target),
        root.path(),
        &repo,
    )
    .await;
    let access = &cycle.access;
    let configured = access
        .execute(
            "{ Node { node_did enabled } DefaultAgentSelection { node_did default_agent_did } Agent { agent_did creation_nonce enabled inference_profile_id } InferenceProfile { profile_id backend_id model_name } WorkspaceRoot { root_path enabled } }",
        )
        .await
        .unwrap();
    assert_eq!(
        configured["data"]["DefaultAgentSelection"][0]["default_agent_did"],
        cycle.agent(WRITER),
        "installed principal/default behavior: {configured:#}"
    );
    for slot in [WRITER, REVIEWER, INTEGRATOR, RECORDER, REJECTER] {
        let binding = &cycle.agents[slot];
        assert!(
            binding.agent_did.starts_with("did:key:"),
            "{slot} is a derived Agent DID: {binding:?}"
        );
        assert!(
            configured["data"]["Agent"]
                .as_array()
                .is_some_and(
                    |rows| rows.iter().any(|row| row["agent_did"] == binding.agent_did
                        && row["creation_nonce"] == binding.creation_nonce)
                ),
            "installed {slot} Agent missing its provisioned identity: {configured:#}"
        );
    }

    let runtime = boot_cycle(&cycle, root.path()).await;

    create_work_unit(access, WORK_UNIT, &base_sha, "accepted").await;

    let writer = wait_for_writer(access, WORK_UNIT, cycle.agent(WRITER)).await;
    assert_eq!(writer.len(), 1, "{writer:#?}");
    assert_eq!(writer[0]["status"], "ready", "{writer:#?}");
    assert_eq!(
        serde_json::from_str::<Vec<String>>(writer[0]["changed_files"].as_str().unwrap()).unwrap(),
        ["src/lib.rs"],
        "{writer:#?}"
    );

    let writer_receipts = wait_receipts(access, WORK_UNIT, "writer", 1).await;
    assert_eq!(writer_receipts.len(), 1, "{writer_receipts:#?}");
    let writer_receipt = &writer_receipts[0];
    assert_eq!(writer_receipt["work_unit_id"], WORK_UNIT);
    assert_eq!(writer_receipt["changed_files"], "[\"src/lib.rs\"]");
    let workspace_id = writer_receipt["workspace_id"].as_str().unwrap();

    let closure_rows = wait_rows(
        access,
        "ChangeUnitClosure",
        WORK_UNIT,
        "_docID closure_id work_unit_id handoff_id reply_session_id attempt implementation_id review_id workspace_id writer_receipt_id writer_seal_hash status",
        &[],
        |rows| rows.iter().any(|row| row["status"] == "accepted"),
    )
    .await;
    let accepted = closure_rows
        .iter()
        .find(|row| row["status"] == "accepted")
        .expect("the independent reviewer accepted the sealed work");
    assert_eq!(accepted["workspace_id"], workspace_id);
    assert_eq!(accepted["writer_receipt_id"], writer_receipt["receipt_id"]);
    assert_eq!(accepted["writer_seal_hash"], writer_receipt["seal_hash"]);

    let review_rows = query_rows(
        access,
        "ChangeUnitReview",
        WORK_UNIT,
        "review_id work_unit_id implementation_id workspace_id writer_receipt_id writer_seal_hash verdict findings summary",
    )
    .await;
    assert_eq!(review_rows.len(), 1, "{review_rows:#?}");
    assert_eq!(review_rows[0]["verdict"], "accepted");
    assert_eq!(
        review_rows[0]["writer_receipt_id"],
        writer_receipt["receipt_id"]
    );
    assert_eq!(
        review_rows[0]["writer_seal_hash"],
        writer_receipt["seal_hash"]
    );

    let integration_receipts = wait_receipts(access, WORK_UNIT, "integrator", 1).await;
    assert_eq!(integration_receipts.len(), 1, "{integration_receipts:#?}");
    let integration_receipt = &integration_receipts[0];
    assert_eq!(integration_receipt["workspace_id"], workspace_id);
    assert_eq!(
        integration_receipt["seal_hash"],
        writer_receipt["seal_hash"]
    );
    assert!(integration_receipt["head_sha"]
        .as_str()
        .is_some_and(|sha| !sha.is_empty()));
    let request_rows = access
        .execute(
            "{ AgentRequest { request_id agent_did lifecycle_state workspace_id workspace_authority workspace_seal_hash } }",
        )
        .await
        .unwrap();
    let requests = request_rows["data"]["AgentRequest"].as_array().unwrap();
    let request_for_receipt = |receipt: &Value| {
        requests
            .iter()
            .find(|request| request["request_id"] == receipt["produced_by_request_id"])
            .unwrap_or_else(|| panic!("request missing for receipt {receipt:#}"))
    };
    let writer_request = request_for_receipt(writer_receipt);
    let review_requests = requests
        .iter()
        .filter(|request| request["agent_did"] == cycle.agent(REVIEWER))
        .collect::<Vec<_>>();
    assert!(!review_requests.is_empty(), "reviewer request exists");
    let integration_request = request_for_receipt(integration_receipt);
    assert_eq!(writer_request["workspace_authority"], "readWrite");
    assert!(review_requests.iter().all(|request| {
        request["request_id"] != writer_request["request_id"]
            && request["workspace_authority"] == "readOnly"
            && request["workspace_id"] == workspace_id
            && request["workspace_seal_hash"] == writer_receipt["seal_hash"]
    }));
    assert_eq!(integration_request["workspace_authority"], "integrate");
    assert_eq!(writer_request["workspace_id"], workspace_id);
    assert_eq!(integration_request["workspace_id"], workspace_id);
    assert_eq!(
        integration_request["workspace_seal_hash"],
        writer_receipt["seal_hash"]
    );
    wait_request_completed(access, integration_request["request_id"].as_str().unwrap()).await;
    let outcome = wait_integrator_outcome(
        access,
        WORK_UNIT,
        accepted["_docID"].as_str().unwrap(),
        integration_request["request_id"].as_str().unwrap(),
    )
    .await;
    assert_eq!(outcome["source_collection"], "ChangeUnitClosure");
    assert_eq!(outcome["source_handoff_id"], WORK_UNIT);
    assert_eq!(outcome["source_doc_id"], accepted["_docID"]);
    assert_eq!(outcome["reply_session_id"], accepted["reply_session_id"]);
    assert_eq!(outcome["attempt"], accepted["attempt"]);
    assert_eq!(outcome["terminal_state"], "completed");
    assert_eq!(
        integration_receipt["produced_by_request_id"], outcome["request_id"],
        "success evidence must name the exact request that produced the integration receipt"
    );
    let integrated_repo = repo.clone();
    let integrated_head = integration_receipt["head_sha"].as_str().unwrap().to_owned();
    let parent = tokio::task::spawn_blocking(move || {
        git(
            &integrated_repo,
            &["rev-parse", &format!("{integrated_head}^")],
        )
    })
    .await
    .unwrap();
    assert_eq!(parent, base_sha);
    assert_eq!(
        tokio::fs::read_to_string(repo.join("src/lib.rs"))
            .await
            .unwrap(),
        "pub fn cycle_result() -> u8 { 7 }\n"
    );

    let result_rows = wait_rows(
        access,
        "ChangeUnitResult",
        WORK_UNIT,
        "result_id work_unit_id handoff_id reply_session_id attempt closure_id implementation_id review_id workspace_id status writer_receipt_id writer_seal_hash integrator_receipt_id integrator_seal_hash head_sha summary failed_request_id failure_stage failure_reason",
        &cycle.agents(&[RECORDER, REJECTER]),
        |rows| {
            assert!(
                rows.iter().all(|row| row["status"] != "failed"),
                "integration recorder reported failure: {rows:#?}"
            );
            rows.iter().any(|row| row["status"] == "integrated")
        },
    )
    .await;
    let result = result_rows
        .iter()
        .find(|row| row["status"] == "integrated")
        .expect("the pack records only the host-confirmed integration");
    assert_eq!(result["writer_receipt_id"], writer_receipt["receipt_id"]);
    assert_eq!(result["writer_seal_hash"], writer_receipt["seal_hash"]);
    assert_eq!(
        result["integrator_receipt_id"],
        integration_receipt["receipt_id"]
    );
    assert_eq!(
        result["integrator_seal_hash"],
        integration_receipt["seal_hash"]
    );
    assert_eq!(result["head_sha"], integration_receipt["head_sha"]);
    assert_eq!(result["handoff_id"], WORK_UNIT);
    assert_eq!(result["reply_session_id"], accepted["reply_session_id"]);
    assert_eq!(result["attempt"], accepted["attempt"]);
    wait_cycle_requests_completed(
        access,
        WORK_UNIT,
        &cycle.agents(&[WRITER, REVIEWER, INTEGRATOR, RECORDER]),
    )
    .await;

    runtime.shutdown().await;
}

#[cfg(feature = "live-e2e")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn change_unit_pack_rejection_records_terminal_result_without_integration() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "gents=info,gents_migration=warn",
        ))
        .try_init();
    let _p2p = super::P2P_E2E_LOCK.lock().await;
    tokio::task::spawn_blocking(ensure_native_fs_runner_for_test)
        .await
        .unwrap();
    let target = live_target().await;
    let (root, repo, base_sha) = repository_fixture().await;
    let cycle = Cycle::install(
        "workspace-change-unit-rejected-cycle",
        InferenceSource::Live(&target),
        root.path(),
        &repo,
    )
    .await;
    update_document(
        &cycle.access, "workspace_cycle.reject_review", Collection::AgentContext,
        &cycle.owner, "change-unit-reviewer-context", |context| {
            let prompt = context["system_prompt"].as_str().unwrap();
            context["system_prompt"] = format!("{prompt} For this rejection-routing test, use verdict rejected and closure status rejected. Record a material finding that this change requires additional operator review. Copy all receipt and work-unit references from the task and read tools.").into();
        },
    ).await;
    let access = &cycle.access;
    let runtime = boot_cycle(&cycle, root.path()).await;

    const REJECTED_UNIT: &str = "change-unit:cycle-rejected";
    create_work_unit(access, REJECTED_UNIT, &base_sha, "rejected").await;
    let implementations = wait_rows(
        access,
        "ChangeUnitImplementation",
        REJECTED_UNIT,
        "implementation_id work_unit_id workspace_id status changed_files",
        &[],
        |rows| rows.iter().any(|row| row["status"] == "ready"),
    )
    .await;
    assert_eq!(implementations.len(), 1);
    let writer_receipts = wait_receipts(access, REJECTED_UNIT, "writer", 1).await;
    assert_eq!(writer_receipts.len(), 1);
    let rejected_reviews = wait_rows(
        access,
        "ChangeUnitReview",
        REJECTED_UNIT,
        "review_id work_unit_id implementation_id workspace_id writer_receipt_id writer_seal_hash verdict findings summary",
        &cycle.agents(&[REVIEWER]),
        |rows| rows.iter().any(|row| row["verdict"] == "rejected"),
    )
    .await;
    assert_eq!(rejected_reviews.len(), 1);
    let closures = wait_rows(
        access,
        "ChangeUnitClosure",
        REJECTED_UNIT,
        "closure_id work_unit_id handoff_id reply_session_id attempt implementation_id review_id workspace_id writer_receipt_id writer_seal_hash status",
        &[],
        |rows| rows.iter().any(|row| row["status"] == "rejected"),
    )
    .await;
    assert_eq!(closures.len(), 1);
    let results = wait_rejected_result(access, REJECTED_UNIT).await;
    assert_eq!(
        results.len(),
        1,
        "rejected closure is terminal: {results:#?}"
    );
    wait_cycle_requests_completed(
        access,
        REJECTED_UNIT,
        &cycle.agents(&[WRITER, REVIEWER, REJECTER]),
    )
    .await;
    assert!(results[0]["integrator_receipt_id"].is_null());
    assert!(results[0]["integrator_seal_hash"].is_null());
    assert!(results[0]["head_sha"].is_null());
    assert_eq!(
        results[0]["writer_receipt_id"],
        writer_receipts[0]["receipt_id"]
    );
    assert_eq!(
        results[0]["writer_seal_hash"],
        writer_receipts[0]["seal_hash"]
    );
    assert_eq!(results[0]["handoff_id"], REJECTED_UNIT);
    assert_eq!(results[0]["reply_session_id"], "workspace-cycle-lead");
    assert_eq!(results[0]["attempt"], 1);
    let owned_repo = repo.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || git(&owned_repo, &["rev-parse", "HEAD"]))
            .await
            .unwrap(),
        base_sha
    );
    assert_eq!(
        tokio::fs::read_to_string(repo.join("src/lib.rs"))
            .await
            .unwrap(),
        "pub fn cycle_result() -> u8 { 0 }\n"
    );
    let integrator_receipts = access
        .execute(&format!(
            "{{ WorkspaceReceipt(filter: {{work_unit_id: {{_eq: \"{}\"}}, kind: {{_eq: \"integrator\"}}}}) {{ receipt_id }} }}",
            escape_graphql_string(REJECTED_UNIT)
        ))
        .await
        .unwrap();
    assert!(integrator_receipts["data"]["WorkspaceReceipt"]
        .as_array()
        .unwrap()
        .is_empty());
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_stage_outcome_reaches_the_existing_supervisor_session() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "gents=info,gents_migration=warn",
        ))
        .try_init();
    let _p2p = super::P2P_E2E_LOCK.lock().await;
    tokio::task::spawn_blocking(ensure_native_fs_runner_for_test)
        .await
        .unwrap();
    let backend = failure_inbox_backend();
    let (root, repo, base_sha) = repository_fixture().await;
    let cycle = Cycle::install(
        "workspace-change-unit-failure-inbox",
        InferenceSource::Scripted(backend.endpoint()),
        root.path(),
        &repo,
    )
    .await;
    let (owner, access) = (cycle.owner.clone(), &cycle.access);
    disable_pack_writer_trigger(access, &owner).await;

    let supervisor = install_failure_inbox_triggers(&cycle).await;
    select_default_agent(access, &owner, cycle.agent(WRITER)).await;
    let supervisor_session = "workspace-cycle-lead";
    let created_at = crate::support::agent_requests::canonical_now();
    gents_session::request::lifecycle::reserve_goal_session(
        access,
        &gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "workspace-cycle-lead-opening",
            &owner,
            cycle.user.did(),
            &supervisor,
            supervisor_session,
            "Supervise the change unit",
            "interactive",
            &created_at,
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                cycle.user.did(),
            ),
        ),
    )
    .await
    .expect("open the existing supervisor session as the User");

    let runtime = boot_cycle(&cycle, root.path()).await;

    create_work_unit(access, FAILURE_UNIT, &base_sha, "inbox").await;
    let callback = wait_callback_result(access, FAILURE_UNIT).await;
    assert_eq!(callback["work_unit_id"], FAILURE_UNIT);
    let failed = wait_failure_outcome(access, FAILURE_UNIT).await;
    assert_eq!(failed["source_collection"], "CallbackResult");
    assert_eq!(failed["source_doc_id"], callback["_docID"]);
    assert_eq!(failed["source_handoff_id"], FAILURE_UNIT);
    assert_eq!(failed["reply_session_id"], supervisor_session);
    assert_eq!(failed["attempt"], 1);
    assert_eq!(failed["trigger_id"], "change-unit-test-failure");
    assert_eq!(failed["terminal_state"], "failed");

    let inbox = wait_supervisor_inbox(access, FAILURE_UNIT).await;
    assert_eq!(inbox["session_id"], supervisor_session);
    assert_eq!(inbox["agent_did"], supervisor);
    assert_eq!(inbox["requester_did"], cycle.user.did());
    assert_eq!(inbox["lifecycle_state"], "completed");
    assert!(inbox["content"].as_str().unwrap().contains(FAILURE_UNIT));

    let outcome_count = access
        .execute(&format!(
            "{{ FireOutcome(filter: {{ trigger_id: {{ _eq: \"change-unit-supervisor-inbox\" }}, source_handoff_id: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
            escape_graphql_string(FAILURE_UNIT)
        ))
        .await
        .unwrap();
    assert!(outcome_count["data"]["FireOutcome"]
        .as_array()
        .unwrap()
        .is_empty());
    runtime.shutdown().await;
}

fn ensure_native_fs_runner_for_test() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("gents manifest should be under workspace crates/");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let current = std::env::current_exe().expect("integration-test binary path");
        let parent = current.parent().expect("integration-test binary directory");
        let profile_dir = parent.parent().expect("Cargo profile directory");
        let profile = profile_dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("Cargo profile directory has a name");
        let profile = if profile == "debug" { "dev" } else { profile };
        let status = Command::new(cargo)
            .args(["build", "-p", "gents-fs-runner"])
            .args(["--profile", profile])
            .arg("--target-dir")
            .arg(profile_dir.parent().expect("Cargo target directory"))
            .current_dir(repo_root)
            .status()
            .expect("building native filesystem runner for workspace cycle test");
        assert!(
            status.success(),
            "gents-fs-runner must build for workspace cycle test"
        );
        let runner_name = if cfg!(windows) {
            "gents-fs-runner.exe"
        } else {
            "gents-fs-runner"
        };
        assert!(
            [
                parent.to_path_buf(),
                parent
                    .parent()
                    .expect("Cargo profile directory")
                    .to_path_buf()
            ]
            .into_iter()
            .any(|directory| directory.join(runner_name).is_file()),
            "gents-fs-runner test binary must be adjacent to integration-test binary after build"
        );
    });
}

async fn apply_documents(
    access: &ConfigAccess,
    operation: &'static str,
    documents: Vec<(Collection, Value)>,
) {
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact_write(operation, |txn| {
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
        .unwrap_or_else(|error| {
            panic!(
                "{operation}: {}",
                gents_protocol::error_chain::ErrorChain(error.as_ref())
            )
        });
}

async fn update_document(
    access: &ConfigAccess,
    operation: &'static str,
    collection: Collection,
    owner: &str,
    id: &str,
    edit: impl Fn(&mut Value),
) {
    let (_, mut document) = access
        .transact_readonly(operation, |txn| {
            Box::pin(
                async move { read_desired_state_record_in_txn(txn, collection, owner, id).await },
            )
        })
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{operation}: {collection:?} {id} is not published"));
    edit(&mut document);
    apply_documents(access, operation, vec![(collection, document)]).await;
}

async fn select_default_agent(access: &ConfigAccess, owner: &str, agent_did: &str) {
    apply_documents(
        access,
        "workspace_cycle.select_default_agent",
        vec![(
            Collection::DefaultAgentSelection,
            json!({"node_did": owner, "default_agent_did": agent_did}),
        )],
    )
    .await;
}

fn failure_inbox_backend() -> MockStreamingBackend {
    let unit = StreamPlan::current_authored_user(
        "change-unit:cycle-inbox",
        vec![
            StreamResponse::bad_request(
                "scripted failure for the change-unit supervisor inbox test",
            ),
            StreamResponse::completes(
                "change-unit:cycle-inbox",
                ["The failed stage reached the supervisor session."],
            ),
        ],
    );
    MockStreamingBackend::start_with_plans(MODEL, vec![unit]).expect("mock provider starts")
}

async fn install_inference(access: &ConfigAccess, owner: &str, source: InferenceSource<'_>) {
    let execution = json!({
        "node_did":owner,"execution_id":"workspace-cycle-execution","max_turns":8,
        "deadline_duration_secs":60,"stream_liveness_timeout_secs":10,"stream_batch_ms":5
    });
    let mut docs = vec![(Collection::InferenceExecution, execution)];
    let (backend, profile, sampling) = match source {
        InferenceSource::Scripted(endpoint) => (
            json!({"node_did":owner,"backend_id":"workspace-cycle-backend","name":"Workspace cycle failure fixture",
                "provider_kind":"OpenAiCompatible","openai_wire_api":"chat_completions","endpoint":endpoint,
                "auth":{"kind":"unauthenticated"},"enabled":true,"max_concurrent":1,"max_queue_depth":8}),
            json!({"node_did":owner,"profile_id":"workspace-cycle-profile","backend_id":"workspace-cycle-backend",
                "model_name":MODEL,"context_window":16384,"max_output_tokens":4096,
                "execution_id":"workspace-cycle-execution","sampling_id":"workspace-cycle-sampling"}),
            Some(
                json!({"node_did":owner,"sampling_id":"workspace-cycle-sampling","temperature":0.0}),
            ),
        ),
        #[cfg(feature = "live-e2e")]
        InferenceSource::Live(target) => {
            let mut backend = target.backend(owner);
            backend.backend_id = "workspace-cycle-backend".into();
            let mut profile = target.profile(owner);
            profile.profile_id = "workspace-cycle-profile".into();
            profile.backend_id = backend.backend_id.clone();
            profile.execution_id = Some("workspace-cycle-execution".into());
            (
                serde_json::to_value(backend).unwrap(),
                serde_json::to_value(profile).unwrap(),
                target
                    .sampling(owner)
                    .map(|value| serde_json::to_value(value).unwrap()),
            )
        }
    };
    docs.extend([
        (Collection::InferenceBackend, backend),
        (Collection::InferenceProfile, profile),
    ]);
    if let Some(sampling) = sampling {
        docs.push((Collection::InferenceSampling, sampling));
    }
    apply_documents(access, "workspace_cycle.install_inference", docs).await;
}

async fn install_pack(access: &ConfigAccess, owner: &str, repo: &Path) -> PackAgentBindings {
    let archive = tokio::task::spawn_blocking(|| {
        let (bytes, _) = pack_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace_cycle_pack"),
        )
        .unwrap();
        PackArchive::from_bytes(&bytes).unwrap()
    })
    .await
    .unwrap();
    let root = repo.to_string_lossy().into_owned();
    let environment = move |name: &str| (name == "GENTS_CHANGE_UNIT_ROOT").then(|| root.clone());
    let inference = BTreeMap::from([
        ("writer".to_string(), "workspace-cycle-profile".to_string()),
        (
            "reviewer".to_string(),
            "workspace-cycle-profile".to_string(),
        ),
        (
            "integrator".to_string(),
            "workspace-cycle-profile".to_string(),
        ),
    ]);
    let prepared = prepare_document_pack_archive(
        access,
        owner,
        &archive,
        &inference,
        &[],
        InstalledVia::Operator,
        None,
        &environment,
        &Default::default(),
    )
    .await
    .expect("prepare the change-unit pack as the User");
    let expected_root = repo.to_string_lossy();
    for id in ["change-unit-writer-tools", "change-unit-review-tools"] {
        let tools = prepared
            .config
            .tools
            .iter()
            .find(|tools| tools.tools_id == id)
            .unwrap_or_else(|| panic!("pack is missing {id}"));
        assert_eq!(
            tools.host.as_ref().and_then(|host| host.root.as_deref()),
            Some(expected_root.as_ref()),
            "the pack loader must resolve {id} from GENTS_CHANGE_UNIT_ROOT"
        );
    }
    install_prepared_document_pack(
        access,
        owner,
        &archive,
        &[],
        &prepared,
        None,
        false,
        InstalledVia::Operator,
        DriftPolicy::Refuse,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .expect("canonical pack installer accepts the unit pack");
    prepared.agent_bindings
}

async fn install_workspace_root(cycle: &Cycle, root: &Path) {
    let readers = cycle
        .agents(&[WRITER, REVIEWER, INTEGRATOR, RECORDER, REJECTER])
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    gents_tools::surface::set_workspace_root_with_readers(
        &cycle.access,
        &cycle.owner,
        root.to_str().unwrap(),
        Some("Change cycle fixture"),
        true,
        &readers,
    )
    .await
    .expect("publish the workspace root as the User");
}

async fn disable_pack_writer_trigger(access: &ConfigAccess, owner: &str) {
    update_document(
        access,
        "workspace_cycle.disable_pack_writer_trigger",
        Collection::Trigger,
        owner,
        "change-unit-write",
        |trigger| trigger["enabled"] = false.into(),
    )
    .await;
}

async fn install_failure_inbox_triggers(cycle: &Cycle) -> String {
    let (owner, writer) = (cycle.owner.as_str(), cycle.agent(WRITER));
    let supervisor = gents_store::identity::provision_agent_identity_authorized(
        &cycle.db.node,
        cycle.db.node_identity.as_ref(),
        &identity::Did::new(cycle.user.did()).unwrap(),
    )
    .await
    .expect("provision the supervisor Agent for the User");
    let config = json!({
        "node": {"node_did": owner},
        "default_agent_selection": {"node_did": owner, "default_agent_did": writer},
        "agents": [
            {
                "node_did": owner,
                "agent_did": supervisor.agent_did,
                "creation_nonce": supervisor.creation_nonce,
                "display_name": "Test Supervisor Inbox",
                "inference_profile_id": "workspace-cycle-profile"
            }
        ],
        "tasks": [
            {
                "node_did": owner,
                "task_id": "change-unit-test-failure-task",
                "agent_did": writer,
                "prompt_template": "Test failure stage for {{ doc.work_unit_id }}.",
                "emit_outcome": true,
                "outcome_readers": [{"agent_did": supervisor.agent_did}]
            },
            {
                "node_did": owner,
                "task_id": "change-unit-supervisor-inbox-task",
                "agent_did": supervisor.agent_did,
                "prompt_template": "Supervisor inbox delivery: stage {{ doc.trigger_id }} for {{ doc.source_handoff_id }} failed: {{ doc.reason }}.",
                "emit_outcome": false
            }
        ],
        "event_sources": [
            {
                "node_did": owner,
                "event_source_id": "change-unit-test-failure-source",
                "source_collection": "CallbackResult",
                "event_kind": "created",
                "correlation_field": "work_unit_id",
                "workspace_authority": "readWrite",
                "filter": "{ binding_id: { _eq: \"change-unit-workspace\" }, work_unit_id: { _eq: \"change-unit:cycle-inbox\" } }"
            },
            {
                "node_did": owner,
                "event_source_id": "change-unit-supervisor-inbox-source",
                "source_collection": "FireOutcome",
                "event_kind": "created",
                "correlation_field": "source_handoff_id",
                "filter": "{ source_collection: { _eq: \"CallbackResult\" }, terminal_state: { _eq: \"failed\" }, trigger_id: { _eq: \"change-unit-test-failure\" }, source_handoff_id: { _eq: \"change-unit:cycle-inbox\" } }"
            }
        ],
        "triggers": [
            {
                "node_did": owner,
                "trigger_id": "change-unit-test-failure",
                "task_id": "change-unit-test-failure-task",
                "source": {"kind": "event", "event_source_id": "change-unit-test-failure-source"},
                "concurrency": "parallel"
            },
            {
                "node_did": owner,
                "trigger_id": "change-unit-supervisor-inbox",
                "task_id": "change-unit-supervisor-inbox-task",
                "source": {"kind": "event", "event_source_id": "change-unit-supervisor-inbox-source"},
                "session_id_template": "{{ doc.reply_session_id }}",
                "concurrency": "queued_serial"
            }
        ]
    });
    let config: gents_documents::PackConfig =
        serde_json::from_value(config).expect("test event and Task documents deserialize");
    let plan = DesiredStateApplyPlan::from_pack_config(&config)
        .expect("test event and Task documents validate");
    cycle
        .access
        .transact_write("workspace_cycle.install_failure_inbox_triggers", |txn| {
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
        .expect("install test-only failure outcome and inbox triggers");
    supervisor.agent_did
}

fn create_repository(repo: &Path) -> (String, String) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    gents_session::workspace::init_test_repo(repo, &["-b", "main"]);
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn cycle_result() -> u8 { 0 }\n",
    )
    .unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "workspace cycle base"]);
    (
        git(repo, &["rev-parse", "HEAD"]),
        git(repo, &["rev-parse", "HEAD^{tree}"]),
    )
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn create_work_unit(
    access: &ConfigAccess,
    work_unit_id: &str,
    base_sha: &str,
    attempt: &str,
) {
    let branch = format!("cycle-{attempt}");
    let sdl = tokio::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/workspace_cycle_pack/schemas/change_unit_work.graphql"),
    )
    .await
    .unwrap();
    gents_store::config_client::create_authored_document(
        access,
        "ChangeUnitWork",
        &json!({
            "work_unit_id": work_unit_id,
            "repository_id": "change-unit-repository",
            "base_sha": base_sha,
            "branch": branch,
            "title": "Workspace change cycle",
            "instructions": "Read src/lib.rs, then call write_file to replace its content with exactly `pub fn cycle_result() -> u8 { 7 }\n`. Run bash with command git, args [diff, --check], cwd . and raw_json true. Then write_change_unit_implementation with status ready, changed_files set to a string encoding the single-element JSON array containing src/lib.rs, and truthful tests_run and summary. Finish after the implementation is durable.",
            "owned_files": "[\"src/lib.rs\"]",
            "status": "ready",
            "caused_by_correlation": work_unit_id,
            "handoff_id": work_unit_id,
            "reply_session_id": "workspace-cycle-lead",
            "attempt": 1,
        }),
        gents_store::config_client::DocumentSchemaSource::UserAuthored,
        &sdl,
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "create work unit {work_unit_id} as the User: {}",
            gents_protocol::error_chain::ErrorChain(&error)
        )
    });
}

#[cfg(feature = "live-e2e")]
async fn wait_integrator_outcome(
    access: &ConfigAccess,
    work_unit_id: &str,
    closure_doc_id: &str,
    request_id: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut changes = access.subscribe_changes("FireOutcome").await;
    loop {
        let query = format!(
            "{{ FireOutcome(filter: {{ source_handoff_id: {{ _eq: \"{}\" }}, source_doc_id: {{ _eq: \"{}\" }}, request_id: {{ _eq: \"{}\" }} }}) {{ source_collection source_doc_id request_id source_handoff_id reply_session_id attempt terminal_state trigger_id }} }}",
            escape_graphql_string(work_unit_id),
            escape_graphql_string(closure_doc_id),
            escape_graphql_string(request_id),
        );
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["FireOutcome"].as_array().unwrap();
        if let Some(row) = rows.first() {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "completed integrator FireOutcome did not appear for {work_unit_id}/{closure_doc_id}/{request_id}: {response:#}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

#[cfg(feature = "live-e2e")]
async fn query_rows(
    access: &ConfigAccess,
    collection: &str,
    work_unit_id: &str,
    fields: &str,
) -> Vec<Value> {
    let id = escape_graphql_string(work_unit_id);
    let query =
        format!("{{ {collection}(filter: {{work_unit_id: {{_eq: \"{id}\"}}}}) {{{fields}}} }}");
    let response = access.execute(&query).await.unwrap();
    response["data"][collection]
        .as_array()
        .unwrap_or_else(|| panic!("missing {collection} rows in {response:#}"))
        .clone()
}

#[cfg(feature = "live-e2e")]
async fn wait_rows(
    access: &ConfigAccess,
    collection: &str,
    work_unit_id: &str,
    fields: &str,
    writing_agents: &[&str],
    ready: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut changes = access.subscribe_changes(collection).await;
    let mut request_changes = access.subscribe_changes("AgentRequest").await;
    loop {
        let rows = query_rows(access, collection, work_unit_id, fields).await;
        if ready(&rows) {
            return rows;
        }
        if !writing_agents.is_empty() {
            let failures = cycle_failure_diagnostics(access, work_unit_id, writing_agents).await;
            if !failures.is_empty() {
                panic!("cycle request terminated before writing {collection}: {failures:#?}");
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {collection} {work_unit_id}; last rows: {rows:#?}"
        );
        let _ = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                _ = changes.recv() => (),
                _ = request_changes.recv() => (),
            }
        })
        .await;
    }
}

#[cfg(feature = "live-e2e")]
async fn cycle_failure_diagnostics(
    access: &ConfigAccess,
    work_unit_id: &str,
    agents: &[&str],
) -> Vec<Value> {
    let work_unit_id = escape_graphql_string(work_unit_id);
    let requests = access
        .execute(
            &format!(
                r#"{{ AgentRequest(filter: {{ caused_by_correlation: {{ _eq: "{work_unit_id}" }} }}) {{ _docID request_id node_did agent_did requester_did session_id lifecycle_state failure_reason caused_by_trigger_id }} }}"#
            ),
        )
        .await
        .ok()
        .and_then(|response| response.get("data").cloned())
        .and_then(|data| data.get("AgentRequest").cloned())
        .and_then(|rows| rows.as_array().cloned())
        .unwrap_or_default();
    let mut failures = Vec::new();
    for request in requests {
        if !agents.contains(&request["agent_did"].as_str().unwrap_or_default()) {
            continue;
        }
        if !matches!(
            request["lifecycle_state"].as_str(),
            Some("failed" | "interrupted" | "completed")
        ) {
            continue;
        }
        let Some(request_id) = request["request_id"].as_str() else {
            continue;
        };
        let id = escape_graphql_string(request_id);
        let tools = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ request_id: {{ _eq: "{id}" }} }}) {{ _docID tool_name lifecycle_state tool_failure_class denial_reason }} }}"#
            ))
            .await
            .ok()
            .and_then(|response| response.get("data").cloned())
            .and_then(|data| data.get("AgentToolCall").cloned())
            .unwrap_or(Value::Null);
        let mut tool_results = Vec::new();
        if let Some(rows) = tools.as_array() {
            for tool in rows
                .iter()
                .filter(|tool| tool["lifecycle_state"] == "failed")
            {
                let (Some(tool_doc_id), Some(owner), Some(session_id)) = (
                    tool["_docID"].as_str(),
                    request["node_did"].as_str(),
                    request["session_id"].as_str(),
                ) else {
                    continue;
                };
                let result = gents_session::tool_call::query::result::load_tool_call_result(
                    access,
                    tool_doc_id,
                    owner,
                    session_id,
                    request["requester_did"].as_str(),
                )
                .await
                .and_then(|message| {
                    gents_session::tool_call::query::result::render_tool_result(&message)
                });
                tool_results.push(json!({
                    "tool": tool,
                    "result": result.unwrap_or_else(|error| format!("<unavailable: {error:#}>") )
                }));
            }
        }
        failures.push(
            json!({ "request": request, "tool_calls": tools, "failed_tool_results": tool_results }),
        );
    }
    failures
}

#[cfg(feature = "live-e2e")]
async fn wait_for_writer(access: &ConfigAccess, work_unit_id: &str, writer: &str) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut changes = access.subscribe_changes("ChangeUnitImplementation").await;
    let mut request_changes = access.subscribe_changes("AgentRequest").await;
    loop {
        let rows = query_rows(
            access,
            "ChangeUnitImplementation",
            work_unit_id,
            "implementation_id work_unit_id workspace_id status changed_files tests_run summary",
        )
        .await;
        if !rows.is_empty() {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "writer implementation missing for {work_unit_id}: {:?}",
            cycle_failure_diagnostics(access, work_unit_id, &[writer]).await
        );
        let _ = tokio::time::timeout_at(deadline, async {
            tokio::select! {
                _ = changes.recv() => (),
                _ = request_changes.recv() => (),
            }
        })
        .await;
    }
}

#[cfg(feature = "live-e2e")]
async fn wait_receipts(
    access: &ConfigAccess,
    work_unit_id: &str,
    kind: &str,
    count: usize,
) -> Vec<Value> {
    let kind = escape_graphql_string(kind);
    let fields = format!(
        "{{ WorkspaceReceipt(filter: {{work_unit_id: {{_eq: \"{}\"}}, kind: {{_eq: \"{kind}\"}}}}) {{ receipt_id produced_by_request_id workspace_id work_unit_id kind base_sha seal_hash head_sha changed_files }} }}",
        escape_graphql_string(work_unit_id),
    );
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut changes = access.subscribe_changes("WorkspaceReceipt").await;
    loop {
        let response = access.execute(&fields).await.unwrap();
        let rows = response["data"]["WorkspaceReceipt"]
            .as_array()
            .unwrap()
            .clone();
        if rows.len() >= count {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {kind} receipt of {work_unit_id}: {response:#}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

async fn wait_callback_result(access: &ConfigAccess, work_unit_id: &str) -> Value {
    let work_unit_id = escape_graphql_string(work_unit_id);
    let query = format!(
        "{{ CallbackResult(filter: {{ work_unit_id: {{ _eq: \"{work_unit_id}\" }} }}) {{ _docID binding_id work_unit_id workspace_id }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut changes = access.subscribe_changes("CallbackResult").await;
    loop {
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["CallbackResult"].as_array().unwrap();
        if let Some(row) = rows.first() {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "workspace callback did not produce CallbackResult for {work_unit_id}: {response:#}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

async fn wait_failure_outcome(access: &ConfigAccess, work_unit_id: &str) -> Value {
    let work_unit_id = escape_graphql_string(work_unit_id);
    let query = format!(
        "{{ FireOutcome(filter: {{ trigger_id: {{ _eq: \"change-unit-test-failure\" }}, source_handoff_id: {{ _eq: \"{work_unit_id}\" }} }}) {{ _docID source_collection source_doc_id request_id source_handoff_id reply_session_id attempt terminal_state trigger_id reason }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut changes = access.subscribe_changes("FireOutcome").await;
    loop {
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["FireOutcome"].as_array().unwrap();
        if let Some(row) = rows.first() {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "failed stage did not publish a FireOutcome for {work_unit_id}: {response:#}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

async fn wait_supervisor_inbox(access: &ConfigAccess, work_unit_id: &str) -> Value {
    let work_unit_id = escape_graphql_string(work_unit_id);
    let query = format!(
        "{{ AgentRequest(filter: {{ caused_by_trigger_id: {{ _eq: \"change-unit-supervisor-inbox\" }}, caused_by_correlation: {{ _eq: \"{work_unit_id}\" }} }}) {{ request_id requester_did session_id agent_did lifecycle_state failure_reason content }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut changes = access.subscribe_changes("AgentRequest").await;
    loop {
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        if let Some(row) = rows.first() {
            if row["lifecycle_state"] == "completed" {
                return row.clone();
            }
            assert!(
                row["lifecycle_state"] != "failed",
                "supervisor inbox request failed: {row:#}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "failed-stage inbox request was not completed: {response:#}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

#[cfg(feature = "live-e2e")]
async fn wait_request_completed(access: &ConfigAccess, request_id: &str) {
    assert_eq!(
        crate::support::live_inference::wait_for_request_terminal(
            access,
            request_id,
            Duration::from_secs(10),
        )
        .await,
        "completed"
    );
}

#[cfg(feature = "live-e2e")]
async fn wait_rejected_result(access: &ConfigAccess, work_unit_id: &str) -> Vec<Value> {
    let unit = escape_graphql_string(work_unit_id);
    let result_query = format!(
        "{{ ChangeUnitResult(filter: {{work_unit_id: {{_eq: \"{unit}\"}}}}) {{ result_id work_unit_id handoff_id reply_session_id attempt closure_id implementation_id review_id workspace_id status writer_receipt_id writer_seal_hash integrator_receipt_id integrator_seal_hash head_sha summary }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut changes = access.subscribe_changes("ChangeUnitResult").await;
    loop {
        let response = access.execute(&result_query).await.unwrap();
        let rows = response["data"]["ChangeUnitResult"]
            .as_array()
            .unwrap()
            .clone();
        if rows.iter().any(|row| row["status"] == "rejected") {
            return rows;
        }
        if Instant::now() >= deadline {
            let diagnostic = format!(
                "{{ Trigger(filter: {{trigger_id: {{_eq: \"change-unit-rejected\"}}}}) {{ trigger_id last_status last_error fire_count }} TriggerFire(filter: {{trigger_id: {{_eq: \"change-unit-rejected\"}}}}) {{ request_id task_id source_doc_id }} AgentRequest(filter: {{caused_by_correlation: {{_eq: \"{unit}\"}}}}) {{ request_id agent_did lifecycle_state failure_reason }} AgentToolCall {{ request_id tool_name lifecycle_state denial_reason tool_failure_class }} }}"
            );
            let state = access.execute(&diagnostic).await.unwrap();
            panic!("rejected result not written: result={rows:#?}; runtime={state:#}");
        }
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}

#[cfg(feature = "live-e2e")]
async fn wait_cycle_requests_completed(access: &ConfigAccess, work_unit_id: &str, agents: &[&str]) {
    let work_unit_id = escape_graphql_string(work_unit_id);
    let query = format!(
        "{{ AgentRequest(filter: {{caused_by_correlation: {{_eq: \"{work_unit_id}\"}}}}) {{ request_id agent_did lifecycle_state failure_reason }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut changes = access.subscribe_changes("AgentRequest").await;
    loop {
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        let relevant = rows
            .iter()
            .filter(|row| agents.contains(&row["agent_did"].as_str().unwrap_or_default()))
            .collect::<Vec<_>>();
        let observed_agents = relevant
            .iter()
            .filter_map(|row| row["agent_did"].as_str())
            .collect::<rapidhash::RapidHashSet<_>>();
        if agents
            .iter()
            .all(|behavior| observed_agents.contains(behavior))
            && relevant
                .iter()
                .all(|row| row["lifecycle_state"] == "completed")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "cycle requests did not finish for {work_unit_id}: {relevant:#?}"
        );
        let _ = tokio::time::timeout_at(deadline, changes.recv()).await;
    }
}
