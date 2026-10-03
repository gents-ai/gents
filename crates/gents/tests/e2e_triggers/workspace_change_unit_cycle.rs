use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gents::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::graphql::escape_graphql_string;
use gents::pack::{
    bind_pack_install_config, declared_paths, digest_declared_assets, document_pack_schema_paths,
    install_pack_documents, load_pack_config, DriftPolicy, PackIdentity, PackInstallOptions,
    PackManifest,
};
use gents::{Collection, DocumentRuntimeOptions, Gents, ToolCeiling};
use serde_json::{json, Value};

use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse,
};
use crate::support::{interrupt::BootedAgent, test_db};

const WORK_UNIT: &str = "change-unit:cycle-accepted";
const WRITER_TASK_MARK: &str = "Implement one requested change";
const REVIEW_TASK_MARK: &str = "Independently review the sealed writer workspace";
const INTEGRATE_TASK_MARK: &str = "A read-only review accepted sealed work unit";
const RECORD_TASK_MARK: &str = "Record successful host integration";
const RECORD_PLAN: &str = RECORD_TASK_MARK;
const REJECT_TASK_MARK: &str = "Close rejected change unit";
const MODEL: &str = "workspace-cycle-script";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn change_unit_pack_completes_writer_seal_review_and_host_integration() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "gents=info,gents_migration=warn",
        ))
        .try_init();
    let _p2p = super::P2P_E2E_LOCK.lock().await;
    ensure_native_fs_runner_for_test();
    let backend = scripted_backend();
    let db = test_db("workspace-change-unit-cycle").await;
    let owner = db.node_identity.did().to_owned();
    let access = ConfigAccess::Local(db.node.clone());
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("source-repo");
    let (base_sha, base_tree) = create_repository(&repo);

    install_inference(&access, &owner, backend.endpoint()).await;
    install_pack(&access, &owner, &repo).await;
    select_default_behavior(&access, &db.node, &owner).await;
    gents::backend_registry::set_backend_probe_status(
        &db.node,
        &owner,
        "workspace-cycle-backend",
        gents::HEALTHY_PROBE_STATUS,
    )
    .await
    .unwrap();
    install_workspace_root(&access, root.path()).await;
    let configured = access
        .execute(
            "{ AgentPrincipal { agent_did default_behavior_id enabled } AgentBehavior { behavior_id enabled inference_profile_id } InferenceProfile { profile_id backend_id model_name } WorkspaceRoot { root_path enabled } }",
        )
        .await
        .unwrap();
    assert_eq!(
        configured["data"]["AgentPrincipal"][0]["default_behavior_id"], "change-unit-writer",
        "installed principal/default behavior: {configured:#}"
    );
    assert!(
        configured["data"]["AgentBehavior"]
            .as_array()
            .is_some_and(|rows| rows
                .iter()
                .any(|row| row["behavior_id"] == "change-unit-writer")),
        "installed behavior missing: {configured:#}"
    );

    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(root.path()),
            ..Default::default()
        },
    )
    .await
    .expect("the installed change-unit pack routes its tasks");
    assert!(
        agent.unavailable_behaviors().is_empty(),
        "change-unit pack behavior resolution failed: {:#?}",
        agent.unavailable_behaviors()
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut handle = tokio::spawn(agent.run(shutdown_rx));
    tokio::select! {
        result = gents::eval::runner::embedded::wait_for_runtime_ready(&db.node, &owner) => {
            result.expect("runtime ready");
        }
        result = &mut handle => {
            panic!("runtime exited before readiness: {result:?}");
        }
        _ = tokio::time::sleep(Duration::from_secs(20)) => {
            let state = access.execute("{ AgentRuntime { agent_did reconcile_phase last_reconcile_result last_reconcile_error } AgentBehaviorReadiness { agent_did snapshot_json } Trigger { trigger_id enabled last_error } }").await.unwrap();
            panic!("runtime did not publish readiness after 20 seconds; state={state:#}");
        }
    }
    let runtime = BootedAgent::new(shutdown_tx, handle, owner.clone());
    let readiness = crate::support::snapshots::fetch_behavior_readiness_snapshot(&db.node, &owner)
        .await
        .expect("runtime readiness snapshot after initial convergence");
    tracing::info!(?readiness, "workspace cycle runtime readiness");

    create_work_unit(&access, &owner, WORK_UNIT, &base_sha, "accepted").await;

    let writer = wait_for_writer(&access, &backend, WORK_UNIT).await;
    assert_eq!(writer.len(), 1, "{writer:#?}");
    assert_eq!(writer[0]["status"], "ready", "{writer:#?}");
    assert_eq!(
        writer[0]["changed_files"], "[\"src/lib.rs\"]",
        "{writer:#?}"
    );

    let writer_receipts = wait_receipts(&access, WORK_UNIT, "writer", 1).await;
    assert_eq!(writer_receipts.len(), 1, "{writer_receipts:#?}");
    let writer_receipt = &writer_receipts[0];
    assert_eq!(writer_receipt["work_unit_id"], WORK_UNIT);
    assert_eq!(writer_receipt["changed_files"], "[\"src/lib.rs\"]");
    let workspace_id = writer_receipt["workspace_id"].as_str().unwrap();
    let writer_seal_hash = writer_receipt["seal_hash"].as_str().unwrap();
    assert_ne!(writer_seal_hash, base_tree);

    let closure_rows = wait_rows(
        &access,
        "ChangeUnitClosure",
        WORK_UNIT,
        "closure_id work_unit_id implementation_id review_id workspace_id writer_receipt_id writer_seal_hash status",
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
        &access,
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

    let integration_receipts = wait_receipts(&access, WORK_UNIT, "integrator", 1).await;
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
            "{ AgentRequest { request_id behavior_id lifecycle_state workspace_id workspace_authority workspace_seal_hash } }",
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
        .filter(|request| request["behavior_id"] == "change-unit-reviewer")
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
    wait_request_completed(&access, integration_request["request_id"].as_str().unwrap()).await;
    assert_eq!(
        git(
            &repo,
            &[
                "rev-parse",
                &format!("{}^", integration_receipt["head_sha"].as_str().unwrap())
            ]
        ),
        base_sha,
        "host integration commit uses the request's expected base parent"
    );
    let integrated = wait_for_file_contents(
        &repo.join("src/lib.rs"),
        "pub fn cycle_result() -> u8 { 7 }\n",
    )
    .await;
    assert_eq!(integrated, "pub fn cycle_result() -> u8 { 7 }\n");

    let result_plan = wait_for_record_followup(&backend).await;
    let closure = accepted;
    let result_args = json!({
        "result_id": "result-cycle-accepted",
        "closure_id": closure["closure_id"],
        "implementation_id": closure["implementation_id"],
        "review_id": closure["review_id"],
        "status": "integrated",
        "writer_receipt_id": closure["writer_receipt_id"],
        "writer_seal_hash": closure["writer_seal_hash"],
        "summary": "The host integrated the accepted sealed workspace.",
    });
    backend.enqueue_response(
        RECORD_PLAN,
        StreamResponse::streams(
            RECORD_PLAN,
            vec![StreamChunk::tool_call(
                "record-integrated-result",
                "write_change_unit_result",
                result_args.to_string(),
            )],
        ),
    );
    backend.enqueue_response(
        RECORD_PLAN,
        StreamResponse::completes(RECORD_PLAN, ["The host integration receipt is recorded."]),
    );
    let result_rows = wait_rows(
        &access,
        "ChangeUnitResult",
        WORK_UNIT,
        "result_id work_unit_id closure_id implementation_id review_id workspace_id status writer_receipt_id writer_seal_hash integrator_receipt_id integrator_seal_hash head_sha summary",
        |rows| rows.iter().any(|row| row["status"] == "integrated"),
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
    assert!(result_plan);
    wait_cycle_requests_completed(
        &access,
        WORK_UNIT,
        &[
            "change-unit-writer",
            "change-unit-reviewer",
            "change-unit-integrator",
            "change-unit-record",
        ],
    )
    .await;

    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn change_unit_pack_rejection_records_terminal_result_without_integration() {
    let _p2p = super::P2P_E2E_LOCK.lock().await;
    ensure_native_fs_runner_for_test();
    let backend = rejected_backend();
    let db = test_db("workspace-change-unit-rejected-cycle").await;
    let owner = db.node_identity.did().to_owned();
    let access = ConfigAccess::Local(db.node.clone());
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("source-repo");
    let (base_sha, _) = create_repository(&repo);

    install_inference(&access, &owner, backend.endpoint()).await;
    install_pack(&access, &owner, &repo).await;
    select_default_behavior(&access, &db.node, &owner).await;
    gents::backend_registry::set_backend_probe_status(
        &db.node,
        &owner,
        "workspace-cycle-backend",
        gents::HEALTHY_PROBE_STATUS,
    )
    .await
    .unwrap();
    install_workspace_root(&access, root.path()).await;
    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(root.path()),
            ..Default::default()
        },
    )
    .await
    .expect("the installed change-unit pack routes its tasks");
    assert!(agent.unavailable_behaviors().is_empty());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    gents::eval::runner::embedded::wait_for_runtime_ready(&db.node, &owner)
        .await
        .expect("runtime ready");
    let runtime = BootedAgent::new(shutdown_tx, handle, owner.clone());

    const REJECTED_UNIT: &str = "change-unit:cycle-rejected";
    create_work_unit(&access, &owner, REJECTED_UNIT, &base_sha, "rejected").await;
    let implementations = wait_rows(
        &access,
        "ChangeUnitImplementation",
        REJECTED_UNIT,
        "implementation_id work_unit_id workspace_id status changed_files",
        |rows| rows.iter().any(|row| row["status"] == "ready"),
    )
    .await;
    assert_eq!(implementations.len(), 1);
    let writer_receipts = wait_receipts(&access, REJECTED_UNIT, "writer", 1).await;
    assert_eq!(writer_receipts.len(), 1);
    let rejected_reviews = wait_rows(
        &access,
        "ChangeUnitReview",
        REJECTED_UNIT,
        "review_id work_unit_id implementation_id workspace_id writer_receipt_id writer_seal_hash verdict findings summary",
        |rows| rows.iter().any(|row| row["verdict"] == "rejected"),
    )
    .await;
    assert_eq!(rejected_reviews.len(), 1);
    let closures = wait_rows(
        &access,
        "ChangeUnitClosure",
        REJECTED_UNIT,
        "closure_id work_unit_id implementation_id review_id workspace_id writer_receipt_id writer_seal_hash status",
        |rows| rows.iter().any(|row| row["status"] == "rejected"),
    )
    .await;
    assert_eq!(closures.len(), 1);
    wait_for_task_followup(&backend, REJECT_TASK_MARK).await;
    backend.enqueue_response(
        REJECT_TASK_MARK,
        StreamResponse::streams(
            REJECT_TASK_MARK,
            vec![StreamChunk::tool_call(
                "record-rejected-result",
                "write_change_unit_result",
                json!({
                    "result_id": "result-cycle-rejected",
                    "closure_id": closures[0]["closure_id"],
                    "implementation_id": closures[0]["implementation_id"],
                    "review_id": closures[0]["review_id"],
                    "status": "rejected",
                    "writer_receipt_id": writer_receipts[0]["receipt_id"],
                    "writer_seal_hash": writer_receipts[0]["seal_hash"],
                })
                .to_string(),
            )],
        ),
    );
    backend.enqueue_response(
        REJECT_TASK_MARK,
        StreamResponse::completes(REJECT_TASK_MARK, ["The rejection result is durable."]),
    );
    let results = wait_rejected_result(&access, REJECTED_UNIT).await;
    assert_eq!(
        results.len(),
        1,
        "rejected closure is terminal: {results:#?}"
    );
    wait_cycle_requests_completed(
        &access,
        REJECTED_UNIT,
        &[
            "change-unit-writer",
            "change-unit-reviewer",
            "change-unit-rejected",
        ],
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
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), base_sha);
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
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

fn ensure_native_fs_runner_for_test() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("gents manifest should be under workspace crates/");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let status = Command::new(cargo)
            .args(["build", "-p", "gents-fs-runner"])
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
        let current = std::env::current_exe().expect("integration-test binary path");
        let parent = current.parent().expect("integration-test binary directory");
        assert!(
            [
                parent.to_path_buf(),
                parent
                    .parent()
                    .expect("target debug directory")
                    .to_path_buf()
            ]
            .into_iter()
            .any(|directory| directory.join(runner_name).is_file()),
            "gents-fs-runner test binary must be adjacent to integration-test binary after build"
        );
    });
}

async fn select_default_behavior(
    access: &ConfigAccess,
    node: &gents::defra_node::EmbeddedNode,
    owner: &str,
) {
    let mut principal = gents::ensure_agent_principal(node, owner).await.unwrap();
    principal.default_behavior_id = Some("change-unit-writer".to_owned());
    let value = serde_json::to_value(principal).unwrap();
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::AgentPrincipal,
        add: value.clone(),
        update: value,
    }])
    .unwrap();
    access
        .transact("workspace_cycle.select_default_behavior", |txn| {
            let plan = &plan;
            Box::pin(async move { gents::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
}

fn scripted_backend() -> MockStreamingBackend {
    let writer = StreamPlan::current_authored_user(
        WRITER_TASK_MARK,
        vec![
            StreamResponse::streams(
                WRITER_TASK_MARK,
                vec![StreamChunk::tool_call(
                    "write-cycle-file",
                    "write_file",
                    json!({
                        "path": "src/lib.rs",
                        "content": "pub fn cycle_result() -> u8 { 7 }\n",
                        "overwrite": true,
                    })
                    .to_string(),
                )],
            ),
            StreamResponse::streams(
                WRITER_TASK_MARK,
                vec![
                    StreamChunk::tool_call(
                        "check-cycle-diff",
                        "bash",
                        json!({"command":"git","args":["diff","--check"],"cwd":".","raw_json":true}).to_string(),
                    ),
                    StreamChunk::tool_call(
                        "write-cycle-implementation",
                        "write_change_unit_implementation",
                        json!({
                            "implementation_id": "implementation-cycle-accepted",
                            "status": "ready",
                            "changed_files": "[\"src/lib.rs\"]",
                            "tests_run": "git diff --check",
                            "summary": "Updated cycle_result in the owned source file.",
                        })
                        .to_string(),
                    ),
                ],
            ),
            StreamResponse::completes(WRITER_TASK_MARK, ["The implementation is complete."]),
        ],
    );
    let reviewer = StreamPlan::current_authored_user(
        REVIEW_TASK_MARK,
        vec![
            StreamResponse::streams(
                REVIEW_TASK_MARK,
                vec![
                    StreamChunk::tool_call("read-cycle-work", "read_change_unit_work", "{}"),
                    StreamChunk::tool_call(
                        "read-cycle-implementation",
                        "read_change_unit_implementation",
                        "{}",
                    ),
                    StreamChunk::tool_call(
                        "inspect-cycle-diff",
                        "bash",
                        json!({"command":"git","args":["diff","--","src/lib.rs"],"cwd":".","raw_json":true}).to_string(),
                    ),
                    StreamChunk::tool_call(
                        "read-cycle-file",
                        "read_file",
                        json!({"path":"src/lib.rs","raw_json":true}).to_string(),
                    ),
                ],
            ),
            StreamResponse::streams(
                REVIEW_TASK_MARK,
                vec![StreamChunk::tool_call(
                    "write-cycle-review",
                    "write_change_unit_review",
                    json!({
                        "review_id": "review-cycle-accepted",
                        "implementation_id": "implementation-cycle-accepted",
                        "verdict": "accepted",
                        "findings": "[]",
                        "summary": "The exact owned-file change matches the request.",
                    })
                    .to_string(),
                )],
            ),
            StreamResponse::streams(
                REVIEW_TASK_MARK,
                vec![StreamChunk::tool_call(
                    "close-cycle-accepted",
                    "write_change_unit_closure",
                    json!({
                        "closure_id": "closure-cycle-accepted",
                        "implementation_id": "implementation-cycle-accepted",
                        "review_id": "review-cycle-accepted",
                        "status": "accepted",
                    })
                    .to_string(),
                )],
            ),
            StreamResponse::completes(REVIEW_TASK_MARK, ["The read-only review is complete."]),
        ],
    );
    let integrator = StreamPlan::current_authored_user(
        INTEGRATE_TASK_MARK,
        vec![StreamResponse::completes(
            INTEGRATE_TASK_MARK,
            ["Authorize host integration for the accepted closure."],
        )],
    );
    let recorder = StreamPlan::current_authored_user(
        RECORD_TASK_MARK,
        vec![StreamResponse::streams(
            RECORD_TASK_MARK,
            vec![
                StreamChunk::tool_call("read-result-work", "read_change_unit_work", "{}"),
                StreamChunk::tool_call("read-result-closure", "read_change_unit_closure", "{}"),
                StreamChunk::tool_call("read-result-review", "read_change_unit_review", "{}"),
                StreamChunk::tool_call(
                    "read-result-implementation",
                    "read_change_unit_implementation",
                    "{}",
                ),
            ],
        )],
    );
    let backend =
        MockStreamingBackend::start_with_plans(MODEL, vec![writer, reviewer, integrator, recorder])
            .expect("mock provider starts");
    backend.enable_dynamic_followups(RECORD_PLAN);
    backend
}

fn rejected_backend() -> MockStreamingBackend {
    let writer = StreamPlan::current_authored_user(
        WRITER_TASK_MARK,
        vec![
            StreamResponse::streams(
                WRITER_TASK_MARK,
                vec![StreamChunk::tool_call(
                    "write-rejected-cycle-file",
                    "write_file",
                    json!({
                        "path": "src/lib.rs",
                        "content": "pub fn cycle_result() -> u8 { 9 }\n",
                        "overwrite": true,
                    })
                    .to_string(),
                )],
            ),
            StreamResponse::streams(
                WRITER_TASK_MARK,
                vec![StreamChunk::tool_call(
                    "write-rejected-implementation",
                    "write_change_unit_implementation",
                    json!({
                        "implementation_id": "implementation-cycle-rejected",
                        "status": "ready",
                        "changed_files": "[\"src/lib.rs\"]",
                        "tests_run": "git diff --check",
                        "summary": "Prepared a candidate change for independent review.",
                    })
                    .to_string(),
                )],
            ),
            StreamResponse::completes(WRITER_TASK_MARK, ["The candidate is ready for review."]),
        ],
    );
    let reviewer = StreamPlan::current_authored_user(
        REVIEW_TASK_MARK,
        vec![
            StreamResponse::streams(
                REVIEW_TASK_MARK,
                vec![
                    StreamChunk::tool_call("read-rejected-work", "read_change_unit_work", "{}"),
                    StreamChunk::tool_call(
                        "read-rejected-implementation",
                        "read_change_unit_implementation",
                        "{}",
                    ),
                    StreamChunk::tool_call(
                        "write-rejected-review",
                        "write_change_unit_review",
                        json!({
                            "review_id": "review-cycle-rejected",
                            "implementation_id": "implementation-cycle-rejected",
                            "verdict": "rejected",
                            "findings": "[\"The candidate violates the requested API contract.\"]",
                            "summary": "The change must not be integrated.",
                        })
                        .to_string(),
                    ),
                ],
            ),
            StreamResponse::streams(
                REVIEW_TASK_MARK,
                vec![StreamChunk::tool_call(
                    "close-rejected-cycle",
                    "write_change_unit_closure",
                    json!({
                        "closure_id": "closure-cycle-rejected",
                        "implementation_id": "implementation-cycle-rejected",
                        "review_id": "review-cycle-rejected",
                        "status": "rejected",
                    })
                    .to_string(),
                )],
            ),
            StreamResponse::completes(
                REVIEW_TASK_MARK,
                ["The independent review rejected the candidate."],
            ),
        ],
    );
    let reject = StreamPlan::current_authored_user(
        REJECT_TASK_MARK,
        vec![StreamResponse::streams(
            REJECT_TASK_MARK,
            vec![
                StreamChunk::tool_call("read-rejected-closure", "read_change_unit_closure", "{}"),
                StreamChunk::tool_call("read-rejected-review", "read_change_unit_review", "{}"),
            ],
        )],
    );
    let backend = MockStreamingBackend::start_with_plans(MODEL, vec![writer, reviewer, reject])
        .expect("mock rejection provider starts");
    backend.enable_dynamic_followups(REJECT_TASK_MARK);
    backend
}

async fn install_inference(access: &ConfigAccess, owner: &str, endpoint: &str) {
    let docs = [
        (
            Collection::InferenceBackend,
            json!({
                "agent_did": owner,
                "backend_id": "workspace-cycle-backend",
                "name": "Workspace cycle scripted model",
                "provider_kind": "OpenAiCompatible",
                "openai_wire_api": "chat_completions",
                "endpoint": endpoint,
                "auth": {"kind":"unauthenticated"},
                "enabled": true,
                "max_concurrent": 1,
                "max_queue_depth": 8
            }),
        ),
        (
            Collection::InferenceExecution,
            json!({
                "agent_did": owner,
                "execution_id": "workspace-cycle-execution",
                "max_turns": 8,
                "deadline_duration_secs": 60,
                "stream_liveness_timeout_secs": 10,
                "stream_batch_ms": 5
            }),
        ),
        (
            Collection::InferenceSampling,
            json!({
                "agent_did": owner,
                "sampling_id": "workspace-cycle-sampling",
                "temperature": 0.0
            }),
        ),
        (
            Collection::InferenceProfile,
            json!({
                "agent_did": owner,
                "profile_id": "workspace-cycle-profile",
                "display_name": "Workspace cycle scripted model",
                "backend_id": "workspace-cycle-backend",
                "model_name": MODEL,
                "context_window": 16384,
                "max_output_tokens": 4096,
                "execution_id": "workspace-cycle-execution",
                "sampling_id": "workspace-cycle-sampling"
            }),
        ),
    ];
    let plan = DesiredStateApplyPlan::new(
        docs.into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact("workspace_cycle.install_inference", |txn| {
            let plan = &plan;
            Box::pin(async move { gents::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
}

async fn install_pack(access: &ConfigAccess, owner: &str, repo: &Path) {
    let pack_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace_cycle_pack");
    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(pack_dir.join("manifest.json")).unwrap()).unwrap();
    let assets: BTreeMap<_, _> = declared_paths(&manifest)
        .into_iter()
        .map(|path| (path.clone(), std::fs::read(pack_dir.join(&path)).unwrap()))
        .collect();
    let read = |path: &str| {
        assets
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing fixture asset {path}"))
    };
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: owner.to_owned(),
        },
        &read,
        &|name| (name == "GENTS_CHANGE_UNIT_ROOT").then(|| repo.to_string_lossy().into_owned()),
    )
    .unwrap();
    let mut config = config;
    config.agent_principal.default_behavior_id = Some("change-unit-writer".to_owned());

    for path in document_pack_schema_paths(&manifest).unwrap() {
        access
            .add_schema(std::str::from_utf8(assets[path].as_slice()).unwrap())
            .await
            .unwrap();
    }

    let bindings = BTreeMap::from([
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
    let bound = bind_pack_install_config(&manifest, &config, &bindings).unwrap();
    let digest = digest_declared_assets(&manifest, |path| {
        assets
            .get(path)
            .map(Vec::as_slice)
            .ok_or_else(|| anyhow::anyhow!("missing fixture asset {path}"))
    })
    .unwrap();
    let identity = PackIdentity::new(&manifest, digest, Vec::new());
    install_pack_documents(access, owner, &identity, &bound, DriftPolicy::Refuse)
        .await
        .expect("canonical pack installer accepts the unit pack");
}

async fn install_workspace_root(access: &ConfigAccess, root: &Path) {
    let root = escape_graphql_string(root.to_str().unwrap());
    let updated_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{ create_WorkspaceRoot(input: {{
            root_path: "{root}", display_name: "Change cycle fixture", enabled: true,
            updated_at: "{updated_at}"
        }}) {{ _docID }} }}"#
    );
    access
        .write("workspace_cycle.install_workspace_root", &mutation)
        .await
        .unwrap();
}

fn create_repository(repo: &Path) -> (String, String) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.email", "cycle@example.invalid"]);
    git(repo, &["config", "user.name", "Workspace Cycle Test"]);
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
    owner: &str,
    work_unit_id: &str,
    base_sha: &str,
    attempt: &str,
) {
    let branch = format!("cycle-{attempt}");
    let fields = [
        ("work_unit_id", work_unit_id),
        ("repository_id", "change-unit-repository"),
        ("base_sha", base_sha),
        ("branch", branch.as_str()),
        ("title", "Scripted change cycle"),
        (
            "instructions",
            "Set cycle_result to 7 in src/lib.rs and run git diff --check.",
        ),
        ("owned_files", "[\"src/lib.rs\"]"),
        ("status", "ready"),
        ("caused_by_correlation", work_unit_id),
    ];
    let fields = fields
        .into_iter()
        .map(|(key, value)| format!("{key}: \"{}\"", escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    let mutation =
        format!("mutation {{ create_ChangeUnitWork(input: {{{fields}}}) {{ _docID }} }}");
    access
        .write("workspace_cycle.seed_work", &mutation)
        .await
        .unwrap_or_else(|error| panic!("create work unit for {owner}: {error:#}"));
}

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

async fn wait_rows(
    access: &ConfigAccess,
    collection: &str,
    work_unit_id: &str,
    fields: &str,
    ready: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let rows = query_rows(access, collection, work_unit_id, fields).await;
        if ready(&rows) {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {collection} {work_unit_id}; last rows: {rows:#?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_writer(
    access: &ConfigAccess,
    backend: &MockStreamingBackend,
    work_unit_id: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
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
        if Instant::now() >= deadline {
            let state = access
                .execute(
                    "{ AgentRuntime { agent_did reconcile_phase last_reconcile_result last_reconcile_error } AgentBehaviorReadiness { snapshot_json } EventSource { event_source_id source_collection event_kind filter correlation_field workspace_authority } AgentRequest { request_id lifecycle_state failure_reason caused_by_trigger_id caused_by_trigger_kind caused_by_correlation caused_by_source_doc_id } TriggerFire { trigger_id source_collection source_doc_id task_id request_id } Trigger { trigger_id fire_count last_status last_error } CallbackInvocation { callback_id lifecycle_state error caused_by_correlation } CallbackResult { binding_id result_id work_unit_id workspace_id } WorkspaceReceipt { receipt_id kind work_unit_id workspace_id } }",
                )
                .await
                .unwrap();
            panic!(
                "writer implementation missing; model_requests={}, model_bodies={}, state={state:#}",
                backend.observed_requests(WRITER_TASK_MARK),
                backend.observed_completion_bodies().len(),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_record_followup(backend: &MockStreamingBackend) -> bool {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if backend.observed_requests(RECORD_PLAN) >= 2 {
            return true;
        }
        assert!(
            Instant::now() < deadline,
            "record task did not reach its scripted follow-up; completion requests: {}",
            backend.observed_completion_requests()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_task_followup(backend: &MockStreamingBackend, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if backend.observed_requests(marker) >= 2 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{marker} did not reach its scripted follow-up; completion requests: {}",
            backend.observed_completion_requests()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_file_contents(path: &Path, expected: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if contents == expected {
                return contents;
            }
        }
        assert!(
            Instant::now() < deadline,
            "host integration did not update {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_request_completed(access: &ConfigAccess, request_id: &str) {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        "{{ AgentRequest(filter: {{request_id: {{_eq: \"{request_id}\"}}}}) {{ request_id lifecycle_state failure_reason }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        if let Some(row) = rows.first() {
            if row["lifecycle_state"] == "completed" {
                return;
            }
            assert!(
                row["lifecycle_state"] != "failed",
                "request {request_id} failed: {row:#}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "request {request_id} did not complete: {response:#}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_rejected_result(access: &ConfigAccess, work_unit_id: &str) -> Vec<Value> {
    let unit = escape_graphql_string(work_unit_id);
    let result_query = format!(
        "{{ ChangeUnitResult(filter: {{work_unit_id: {{_eq: \"{unit}\"}}}}) {{ result_id work_unit_id closure_id implementation_id review_id workspace_id status writer_receipt_id writer_seal_hash integrator_receipt_id integrator_seal_hash head_sha summary }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(15);
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
                "{{ Trigger(filter: {{trigger_id: {{_eq: \"change-unit-rejected\"}}}}) {{ trigger_id last_status last_error fire_count }} TriggerFire(filter: {{trigger_id: {{_eq: \"change-unit-rejected\"}}}}) {{ request_id task_id source_doc_id }} AgentRequest(filter: {{caused_by_correlation: {{_eq: \"{unit}\"}}}}) {{ request_id behavior_id lifecycle_state failure_reason }} AgentToolCall {{ request_id tool_name lifecycle_state denial_reason tool_failure_class }} }}"
            );
            let state = access.execute(&diagnostic).await.unwrap();
            panic!("rejected result not written: result={rows:#?}; runtime={state:#}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_cycle_requests_completed(
    access: &ConfigAccess,
    work_unit_id: &str,
    behaviors: &[&str],
) {
    let work_unit_id = escape_graphql_string(work_unit_id);
    let query = format!(
        "{{ AgentRequest(filter: {{caused_by_correlation: {{_eq: \"{work_unit_id}\"}}}}) {{ request_id behavior_id lifecycle_state failure_reason }} }}"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let response = access.execute(&query).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        let relevant = rows
            .iter()
            .filter(|row| behaviors.contains(&row["behavior_id"].as_str().unwrap_or_default()))
            .collect::<Vec<_>>();
        let observed_behaviors = relevant
            .iter()
            .filter_map(|row| row["behavior_id"].as_str())
            .collect::<std::collections::HashSet<_>>();
        if behaviors
            .iter()
            .all(|behavior| observed_behaviors.contains(behavior))
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
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
