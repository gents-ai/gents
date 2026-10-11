use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::stream;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use serde_json::json;

use super::AgentDaemon;
use crate::agent::runtime::StartupBarrier;
use crate::config::ResolvedAgent;
use crate::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy};
use crate::llm::tool::ToolDyn;
use crate::prompt::LayeredPromptBuilder;
use crate::watcher::AgentRequest;
use crate::Collection;

const TASK_ID: &str = "hook-task";
const TRIGGER_ID: &str = "hook-trigger";
const WORKSPACE_ID: &str = "task-hook-workspace";

/// Replies once, fails the provider call, or never answers, counting every
/// call.
#[derive(Clone)]
struct ScriptedModel {
    calls: Arc<AtomicUsize>,
    fail: bool,
    hang: bool,
}

#[allow(refining_impl_trait)]
impl CompletionModel for ScriptedModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        unreachable!("task hook daemon tests construct their model directly")
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("unused".into()))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(CompletionError::ProviderError("agent refused".into()));
        }
        if self.hang {
            let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::pending());
            return Ok(StreamingCompletionResponse::stream(inner));
        }
        let inner: rig::streaming::StreamingResult<()> = Box::pin(stream::iter(vec![
            Ok(RawStreamingChoice::Message("hooked reply".to_string())),
            Ok(RawStreamingChoice::FinalResponse(())),
        ]));
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

/// How the request under test reached its Task.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lineage {
    /// A schedule fire resolved through its Trigger.
    Trigger,
    /// A manual `gents task run`: a Task fire receipt written with the request.
    ManualFire,
}

struct Harness {
    node: Arc<defra_node::EmbeddedNode>,
    agent_config: Arc<ResolvedAgent>,
    calls: Arc<AtomicUsize>,
    marks: tempfile::TempDir,
    _data: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        Self::with_deadline(Duration::from_secs(60)).await
    }

    async fn with_deadline(deadline: Duration) -> Self {
        let data = tempfile::tempdir().expect("node data directory");
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(data.path())
                .build()
                .await
                .expect("embedded node"),
        );
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let agent_config = super::inference::tests::test_agent_with_deadline(deadline);
        crate::test_support::install_test_agent(
            node.as_ref(),
            agent_config.node_did(),
            &agent_config.agent_id,
        )
        .await;
        Self {
            node,
            agent_config,
            calls: Arc::new(AtomicUsize::new(0)),
            marks: tempfile::tempdir().expect("hook marker directory"),
            _data: data,
        }
    }

    fn owner(&self) -> &str {
        self.agent_config.node_did()
    }

    fn mark(&self, name: &str) -> PathBuf {
        self.marks.path().join(name)
    }

    fn touch(&self, name: &str) -> String {
        format!("touch {}", self.mark(name).display())
    }

    async fn apply(&self, documents: Vec<(Collection, serde_json::Value)>) {
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
        ConfigAccess::transact_local(self.node.as_ref(), None, "test.task_hooks", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    }

    async fn install_task(&self, hooks: serde_json::Value) {
        let owner = self.owner().to_string();
        self.apply(vec![
            (
                Collection::Schedule,
                json!({
                    "node_did": owner,
                    "schedule_id": "hook-schedule",
                    "cadence": {"kind": "interval", "interval_secs": 3600},
                }),
            ),
            (
                Collection::Task,
                json!({
                    "node_did": owner,
                    "task_id": TASK_ID,
                    "agent_id": self.agent_config.agent_id,
                    "prompt_template": "run the gate",
                    "hooks": hooks,
                }),
            ),
            (
                Collection::Trigger,
                json!({
                    "node_did": owner,
                    "trigger_id": TRIGGER_ID,
                    "task_id": TASK_ID,
                    "source": {"kind": "schedule", "schedule_id": "hook-schedule"},
                }),
            ),
        ])
        .await;
    }

    async fn trigger_doc_id(&self) -> String {
        let response = crate::graphql::graphql_with_transaction_retry(
            self.node.as_ref(),
            &format!(
                r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(self.owner()),
                crate::graphql::escape_graphql_string(TRIGGER_ID),
            ),
            "test.load_task_hook_trigger",
        )
        .await
        .unwrap();
        let row: serde_json::Value = crate::graphql::first_row(&response, "Trigger")
            .unwrap()
            .expect("installed Trigger");
        row["_docID"].as_str().expect("Trigger doc id").to_owned()
    }

    async fn create_request(&self, lineage: Lineage, workspace: bool) -> AgentRequest {
        let owner = self.owner().to_string();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let fire = (lineage == Lineage::ManualFire).then(|| {
            let identity = gents_protocol::trigger_delivery::FireIdentity {
                owner_did: owner.clone(),
                trigger_id: format!("manual:{TASK_ID}"),
                source_collection: "Task".into(),
                source_doc_id: uuid::Uuid::new_v4().to_string(),
            };
            gents_protocol::trigger_delivery::TriggerFire {
                fire_key: crate::lifecycle::task_fire_key(&identity),
                request_id: identity.request_id(),
                session_id: identity.session_id(),
                identity,
                task_id: TASK_ID.into(),
                goal_id: None,
                goal_objective: None,
                goal_token_budget: None,
                goal_assignment_applied: false,
                emit_outcome: false,
                queued_serial: false,
                source_handoff_id: None,
                reply_session_id: None,
                shard_id: None,
                attempt: None,
                created_at: created_at.clone(),
            }
        });
        let (request_id, session_id, admission, origin) = match &fire {
            Some(fire) => (
                fire.request_id.clone(),
                fire.session_id.clone(),
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&owner),
                "interactive",
            ),
            None => (
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_automated_trigger(
                    &owner, TRIGGER_ID,
                ),
                "scheduled",
            ),
        };
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            &owner,
            &owner,
            &self.agent_config.agent_id,
            session_id,
            "run the gate",
            origin,
            created_at,
            admission,
        );
        match &fire {
            Some(fire) => {
                create.caused_by_trigger_kind = Some("manual".into());
                create.retry_key = Some(fire.fire_key.clone());
            }
            None => {
                create.caused_by_trigger_id = Some(TRIGGER_ID.into());
                create.caused_by_trigger_kind = Some("schedule".into());
                create.caused_by_trigger_doc_id = Some(self.trigger_doc_id().await);
            }
        }
        if workspace {
            create.workspace_id = Some(WORKSPACE_ID.to_owned());
            create.workspace_authority = Some("readWrite".into());
            create.workspace_owner_node_did = Some(owner.clone());
        }
        crate::sign_agent_request_create(self.agent_config.node_identity().as_ref(), &mut create)
            .await
            .unwrap();
        let doc_id = match &fire {
            Some(fire) => {
                let access = ConfigAccess::Local(self.node.clone());
                crate::lifecycle::write_task_delivery(&access, fire, false, &create)
                    .await
                    .expect("admit the manual Task fire and its request")
                    .request
                    .doc_id
            }
            None => {
                let response = ConfigAccess::write_local(
                    self.node.as_ref(),
                    "test.create_task_hook_request",
                    &create.graphql_mutation().unwrap(),
                )
                .await
                .expect("create task-hook AgentRequest");
                crate::graphql::created_doc_id(&response, "AgentRequest").unwrap()
            }
        };
        crate::request_admission::load_request_for_admission_test(self.node.as_ref(), &doc_id)
            .await
            .unwrap()
    }

    fn daemon(&self, fail: bool) -> AgentDaemon<ScriptedModel> {
        self.daemon_with(fail, BackgroundExecutionRegistry::default())
    }

    fn daemon_with(
        &self,
        fail: bool,
        executions: BackgroundExecutionRegistry,
    ) -> AgentDaemon<ScriptedModel> {
        self.daemon_model(fail, false, executions)
    }

    fn daemon_model(
        &self,
        fail: bool,
        hang: bool,
        executions: BackgroundExecutionRegistry,
    ) -> AgentDaemon<ScriptedModel> {
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &self.agent_config.system_prompt,
            &self.agent_config.agent_id,
            &[],
            false,
            &[],
        );
        AgentDaemon::new(
            self.node.clone(),
            self.agent_config.clone(),
            None,
            Arc::new(ScriptedModel {
                calls: self.calls.clone(),
                fail,
                hang,
            }),
            prompt_builder.preamble().to_string(),
            Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            FailurePolicy::default(),
            Some(
                crate::rendered_request::defra_rendered_request_capture_factory(self.node.clone()),
            ),
            BackgroundToolRegistry::default(),
            executions,
            Arc::new(StartupBarrier::ready_for_test()),
            crate::runtime_status::RuntimeStatusHandle::new(
                self.node.clone(),
                self.owner().to_string(),
            ),
            1,
            crate::request_admission::AgentRequestAdmissionVerifier::new(
                self.node.clone(),
                self.agent_config.node_identity().clone(),
                crate::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        )
        .unwrap()
    }

    async fn request_row(&self, doc_id: &str) -> serde_json::Value {
        let response = crate::graphql::graphql_with_transaction_retry(
            self.node.as_ref(),
            &format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state failure_reason }} }}"#,
                crate::graphql::escape_graphql_string(doc_id),
            ),
            "test.load_task_hook_request",
        )
        .await
        .unwrap();
        crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .expect("AgentRequest row")
    }

    async fn run(&self, request: AgentRequest, fail: bool) -> serde_json::Value {
        let doc_id = request.doc_id.clone();
        let mut daemon = self.daemon(fail);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::time::timeout(
            Duration::from_secs(90),
            daemon.process_request(request, shutdown_rx),
        )
        .await
        .expect("request processing finished")
        .expect("request processing returned");
        self.request_row(&doc_id).await
    }

    /// Runs the request and interrupts it once `marker` appears.
    async fn run_interrupting_at(&self, request: AgentRequest, marker: &Path) -> serde_json::Value {
        let doc_id = request.doc_id.clone();
        let mut daemon = self.daemon(false);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let interrupt = async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            crate::interrupt::interrupt_request_by_doc_id(
                self.node.as_ref(),
                &doc_id,
                self.owner(),
                Some(self.owner()),
            )
            .await
            .expect("interrupt the running request");
        };
        let (processed, ()) = tokio::time::timeout(Duration::from_secs(90), async {
            tokio::join!(daemon.process_request(request, shutdown_rx), interrupt)
        })
        .await
        .expect("interrupted request processing finished");
        processed.expect("request processing returned");
        self.request_row(&doc_id).await
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn reason(row: &serde_json::Value) -> &str {
    row["failure_reason"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn a_failing_before_hook_stops_the_request_before_the_provider() {
    let harness = Harness::new().await;
    harness
        .install_task(json!([{
            "hook_id": "prepare",
            "phase": "before",
            "command": ["sh", "-c", "echo blocked >&2; exit 1"],
            "timeout_secs": 30,
        }]))
        .await;
    let request = harness.create_request(Lineage::Trigger, false).await;
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(
        reason(&row).contains("prepare") && reason(&row).contains("blocked"),
        "{row}"
    );
}

#[tokio::test]
async fn passing_hooks_leave_a_manual_task_run_completed() {
    let harness = Harness::new().await;
    harness
        .install_task(json!([
            {"hook_id": "prepare", "phase": "before",
             "command": ["sh", "-c", harness.touch("prepared")], "timeout_secs": 30},
            {"hook_id": "verify", "phase": "after_success",
             "command": ["sh", "-c", harness.touch("verified")], "timeout_secs": 30},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.touch("swept")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 1, "{row}");
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    for mark in ["prepared", "verified", "swept"] {
        assert!(harness.mark(mark).exists(), "{mark} hook did not run");
    }
}

#[tokio::test]
async fn a_failing_cleanup_keeps_the_agent_failure_as_the_primary_error() {
    let harness = Harness::new().await;
    harness
        .install_task(json!([
            {"hook_id": "report", "phase": "after_failure",
             "command": ["sh", "-c", harness.touch("reported")], "timeout_secs": 30},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", "exit 1"], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let row = harness.run(request, true).await;
    assert!(harness.calls() > 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    let reason = reason(&row);
    assert!(!reason.starts_with("task hook"), "{reason:?}");
    assert!(
        reason.contains("task cleanup hooks failed: sweep"),
        "{reason:?}"
    );
    assert!(harness.mark("reported").exists());
}

#[tokio::test]
async fn interrupting_a_held_after_success_hook_cancels_it_and_interrupts_the_request() {
    let harness = Harness::new().await;
    let held = format!(
        "{}; sleep 30; {}",
        harness.touch("verify-started"),
        harness.touch("verify-finished")
    );
    harness
        .install_task(json!([
            {"hook_id": "verify", "phase": "after_success",
             "command": ["sh", "-c", held], "timeout_secs": 60},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.touch("swept")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let row = harness
        .run_interrupting_at(request, &harness.mark("verify-started"))
        .await;
    assert_eq!(harness.calls(), 1, "{row}");
    assert_eq!(row["lifecycle_state"], "interrupted", "{row}");
    assert!(!harness.mark("verify-finished").exists());
    assert!(
        harness.mark("swept").exists(),
        "an interrupt never skips cleanup"
    );
}

#[tokio::test]
async fn interrupting_a_before_hook_skips_the_agent_and_still_cleans_up() {
    let harness = Harness::new().await;
    let held = format!(
        "{}; sleep 30; {}",
        harness.touch("prepare-started"),
        harness.touch("prepare-finished")
    );
    harness
        .install_task(json!([
            {"hook_id": "prepare", "phase": "before",
             "command": ["sh", "-c", held], "timeout_secs": 60},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.touch("swept")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::Trigger, false).await;
    let row = harness
        .run_interrupting_at(request, &harness.mark("prepare-started"))
        .await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "interrupted", "{row}");
    assert!(!harness.mark("prepare-finished").exists());
    assert!(harness.mark("swept").exists());
}

#[tokio::test]
async fn a_request_bound_to_a_missing_task_fails_closed() {
    let harness = Harness::new().await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("no longer exists"), "{row}");
}

async fn install_workspace(harness: &Harness, host_path: &Path) {
    let owner = harness.owner().to_string();
    let workspace = crate::workspace::IsolatedWorkspaceDoc {
        path_capability: crate::workspace::WorkspacePathCapability::exact_paths(vec![
            "patch.rs".to_string()
        ])
        .unwrap(),
        workspace_id: WORKSPACE_ID.to_string(),
        work_unit_id: TASK_ID.to_string(),
        repository_id: "hook-repo".to_string(),
        base_sha: "0".repeat(40),
        branch: "gents/hook-task".to_string(),
        creation_policy: crate::workspace::CreationPolicy::GitWorktreeDiff
            .as_str()
            .to_string(),
        adapter: crate::workspace::WorkspaceAdapterKind::GitWorktree
            .as_str()
            .to_string(),
        owner_node_did: owner.clone(),
        writer_principal: owner.clone(),
        integrator_principal: owner.clone(),
        instruction_manifest: String::new(),
        seal_hash: None,
        lifecycle_state: "ready".to_string(),
        caused_by_invocation_id: "hook-invocation".to_string(),
        caused_by_correlation: "hook-correlation".to_string(),
    };
    let placement = crate::workspace::WorkspacePlacementDoc {
        workspace_id: WORKSPACE_ID.to_string(),
        owner_node_did: owner,
        host_path: host_path.to_str().expect("utf-8 path").to_string(),
        repository_placement_id: "hook-repo".to_string(),
        adapter: crate::workspace::WorkspaceAdapterKind::GitWorktree
            .as_str()
            .to_string(),
        adapter_version: "gents-workspace-adapter/1".to_string(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "provisioned".to_string(),
        observed_tree_hash: String::new(),
    };
    let updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for mutation in [
        crate::workspace::isolated_workspace_upsert_mutation(&workspace),
        crate::workspace::workspace_placement_upsert_mutation(&placement, &updated_at),
    ] {
        ConfigAccess::write_local(harness.node.as_ref(), "test.task_hook_workspace", &mutation)
            .await
            .expect("install isolated workspace and placement");
    }
}

async fn workspace_bindings(harness: &Harness) -> Vec<crate::workspace::WorkspaceBindingDoc> {
    let response = crate::graphql::graphql_with_transaction_retry(
        harness.node.as_ref(),
        &format!(
            r#"{{ WorkspaceBinding(filter: {{ workspace_id: {{ _eq: "{}" }}, owner_node_did: {{ _eq: "{}" }} }}) {{ binding_id workspace_id request_id request_doc_id authority owner_node_did seal_hash lifecycle_state }} }}"#,
            crate::graphql::escape_graphql_string(WORKSPACE_ID),
            crate::graphql::escape_graphql_string(harness.owner()),
        ),
        "test.task_hook_bindings",
    )
    .await
    .unwrap();
    crate::graphql::rows(&response, "WorkspaceBinding").unwrap()
}

async fn materialize_writer_binding(harness: &Harness, request: &AgentRequest) {
    crate::workspace::materialize_workspace_binding(
        harness.node.as_ref(),
        &request.request_id,
        &request.doc_id,
        harness.owner(),
        &crate::lifecycle::WorkspaceLineage {
            workspace_id: request.workspace_id.clone(),
            workspace_authority: request.workspace_authority.clone(),
            workspace_owner_node_did: request.workspace_owner_node_did.clone(),
            workspace_seal_hash: request.workspace_seal_hash.clone(),
        },
    )
    .await
    .expect("materialize the writer binding");
}

#[tokio::test]
async fn claim_admission_rejection_releases_the_bound_workspace() {
    let harness = Harness::new().await;
    let placement = tempfile::tempdir().expect("workspace placement directory");
    install_workspace(&harness, placement.path()).await;
    harness.install_task(json!([])).await;
    let request = harness.create_request(Lineage::Trigger, true).await;
    materialize_writer_binding(&harness, &request).await;
    assert!(workspace_bindings(&harness)
        .await
        .iter()
        .any(|binding| binding.is_active_read_write()));
    ConfigAccess::write_local(
        harness.node.as_ref(),
        "test.remove_workspace_placement",
        &format!(
            r#"mutation {{ delete_WorkspacePlacement(filter: {{ workspace_id: {{ _eq: "{}" }}, owner_node_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(WORKSPACE_ID),
            crate::graphql::escape_graphql_string(harness.owner()),
        ),
    )
    .await
    .unwrap();
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("workspace"), "{row}");
    assert!(workspace_bindings(&harness)
        .await
        .iter()
        .all(|binding| !binding.is_active()));
}

/// ReadWrite claim admission reaches worker-ticket binding only on hosts with
/// an enforceable WorkspaceWrite sandbox.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn refused_worker_ticket_releases_the_bound_workspace_before_work() {
    use crate::agent::worker_capacity::{
        bind_current_claim, scope_request_capacity, WorkerCapacity, WorkerTicket,
    };

    let harness = Harness::new().await;
    let placement = tempfile::tempdir().expect("workspace placement directory");
    install_workspace(&harness, placement.path()).await;
    harness.install_task(json!([])).await;
    let request = harness.create_request(Lineage::Trigger, true).await;
    materialize_writer_binding(&harness, &request).await;
    let guard = WorkerCapacity::new(1).try_acquire_unbound().unwrap();
    let row = scope_request_capacity(guard, async {
        bind_current_claim(WorkerTicket::new("previous-request", "previous-generation")).unwrap();
        harness.run(request, false).await
    })
    .await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("worker guard"), "{row}");
    assert!(workspace_bindings(&harness)
        .await
        .iter()
        .all(|binding| !binding.is_active()));
}

/// Claim admission refuses a ReadWrite workspace binding on a host with no
/// enforceable WorkspaceWrite sandbox, so this request reaches its hooks only
/// where one exists.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_failing_before_hook_releases_the_bound_workspace() {
    let harness = Harness::new().await;
    let placement = tempfile::tempdir().expect("workspace placement directory");
    install_workspace(&harness, placement.path()).await;
    harness
        .install_task(json!([{"hook_id": "prepare", "phase": "before",
            "command": ["sh", "-c", "exit 1"], "timeout_secs": 30}]))
        .await;
    let request = harness.create_request(Lineage::Trigger, true).await;
    materialize_writer_binding(&harness, &request).await;
    assert!(workspace_bindings(&harness)
        .await
        .iter()
        .any(crate::workspace::WorkspaceBindingDoc::is_active_read_write));
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("prepare"), "{row}");
    let bindings = workspace_bindings(&harness).await;
    assert!(
        !bindings
            .iter()
            .any(crate::workspace::WorkspaceBindingDoc::is_active_read_write),
        "{bindings:?}"
    );
}

impl Harness {
    /// Runs the request against durable task hook records in `records`, and
    /// abandons it the moment `marker` appears, as a runtime crash would.
    async fn crash_at(&self, request: AgentRequest, marker: &Path, records: &Path) {
        let mut daemon = self.daemon_with(
            false,
            BackgroundExecutionRegistry::default().with_task_hook_records(records.to_path_buf()),
        );
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let crashed = async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::select! {
            _ = daemon.process_request(request, shutdown_rx) => panic!("the held hook must not finish"),
            () = crashed => {}
        }
    }

    /// Restarts against the same records: startup recovery until request
    /// recovery has decided the abandoned request, which task hook recovery
    /// waits for, then the recovered cleanup it started.
    async fn restart(&self, records: &Path, doc_id: &str) {
        let registry =
            BackgroundExecutionRegistry::default().with_task_hook_records(records.to_path_buf());
        for _ in 0..40 {
            let outcome = crate::startup_recovery::run_startup_recovery_with_executions(
                &self.node,
                self.owner(),
                &registry,
            )
            .await;
            if outcome.task_hooks.expect("task hook recovery") > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        registry.task_hook_records().wait_for_recoveries().await;
        let row = self.request_row(doc_id).await;
        assert!(
            matches!(row["lifecycle_state"].as_str(), Some("failed")),
            "request recovery must decide the abandoned request: {row}"
        );
        assert!(registry.task_hook_records().list().is_empty());
        let again = crate::startup_recovery::run_startup_recovery_with_executions(
            &self.node,
            self.owner(),
            &registry,
        )
        .await;
        assert!(again.task_hooks.expect("second pass") == 0);
    }

    fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.mark("hooks.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn logging(&self, hook_id: &str, then: &str) -> String {
        format!(
            "printf '%s\\n' {hook_id} >> {}; {then}",
            self.mark("hooks.log").display()
        )
    }
}

#[tokio::test]
async fn a_restart_during_a_before_hook_runs_cleanup_once_without_rerunning_it() {
    let harness = Harness::new().await;
    let held = harness.logging(
        "prepare",
        &format!(
            "{}; sleep 30; {}",
            harness.touch("prepare-started"),
            harness.touch("prepare-finished")
        ),
    );
    harness
        .install_task(json!([
            {"hook_id": "prepare", "phase": "before",
             "command": ["sh", "-c", held], "timeout_secs": 60},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.logging("sweep", "true")], "timeout_secs": 30},
        ]))
        .await;
    let records = harness.mark("task-hooks");
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let doc_id = request.doc_id.clone();
    harness
        .crash_at(request, &harness.mark("prepare-started"), &records)
        .await;
    harness.restart(&records, &doc_id).await;
    assert_eq!(harness.log_lines(), vec!["prepare", "sweep"]);
    assert!(!harness.mark("prepare-finished").exists());
    assert_eq!(harness.calls(), 0);
}

#[tokio::test]
async fn a_restart_during_cleanup_runs_only_the_remaining_cleanup() {
    let harness = Harness::new().await;
    let held = harness.logging(
        "first",
        &format!("{}; sleep 30", harness.touch("first-started")),
    );
    harness
        .install_task(json!([
            {"hook_id": "first", "phase": "finally",
             "command": ["sh", "-c", held], "timeout_secs": 60},
            {"hook_id": "second", "phase": "finally",
             "command": ["sh", "-c", harness.logging("second", "true")], "timeout_secs": 30},
        ]))
        .await;
    let records = harness.mark("task-hooks");
    let request = harness.create_request(Lineage::Trigger, false).await;
    let doc_id = request.doc_id.clone();
    harness
        .crash_at(request, &harness.mark("first-started"), &records)
        .await;
    harness.restart(&records, &doc_id).await;
    assert_eq!(harness.log_lines(), vec!["first", "second"]);
}

#[tokio::test]
async fn losing_the_lease_mid_work_skips_after_failure_and_still_cleans_up() {
    let harness = Harness::new().await;
    harness
        .install_task(json!([
            {"hook_id": "report", "phase": "after_failure",
             "command": ["sh", "-c", harness.touch("reported")], "timeout_secs": 30},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.touch("swept")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let doc_id = request.doc_id.clone();
    let mut daemon = harness.daemon_model(false, true, BackgroundExecutionRegistry::default());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let replace = async {
        while harness.calls() == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        ConfigAccess::write_local(
            harness.node.as_ref(),
            "test.replace_task_hook_generation",
            &format!(
                r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "replacement-generation" }}) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(&doc_id),
            ),
        )
        .await
        .expect("replace the execution generation");
    };
    let (processed, ()) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(daemon.process_request(request, shutdown_rx), replace)
    })
    .await
    .expect("lease loss is observed");
    processed.expect("request processing returned");
    let row = harness.request_row(&doc_id).await;
    assert!(
        !harness.mark("reported").exists(),
        "a lost lease is not this execution's failure: {row}"
    );
    assert!(harness.mark("swept").exists(), "cleanup still runs live");
    assert_eq!(
        row["lifecycle_state"], "processing",
        "the current owner keeps the terminal: {row}"
    );
}

#[tokio::test]
async fn shutdown_before_any_hook_launches_leaves_no_record_to_recover() {
    let harness = Harness::new().await;
    harness
        .install_task(json!([
            {"hook_id": "prepare", "phase": "before",
             "command": ["sh", "-c", harness.logging("prepare", "true")], "timeout_secs": 30},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.logging("sweep", "true")], "timeout_secs": 30},
        ]))
        .await;
    let records = harness.mark("task-hooks");
    let executions = BackgroundExecutionRegistry::default().with_task_hook_records(records.clone());
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let mut daemon = harness.daemon_model(false, false, executions.clone());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(true);
    daemon
        .process_request(request, shutdown_rx)
        .await
        .expect("request processing returned");
    assert!(harness.log_lines().is_empty(), "no hook launched");
    assert!(harness.calls() == 0);
    assert!(
        executions.task_hook_records().list().is_empty(),
        "an execution that never started leaves nothing for recovery"
    );
    let restarted = BackgroundExecutionRegistry::default().with_task_hook_records(records);
    let outcome = crate::startup_recovery::run_startup_recovery_with_executions(
        &harness.node,
        harness.owner(),
        &restarted,
    )
    .await;
    assert!(outcome.task_hooks.unwrap() == 0);
}

#[tokio::test]
async fn a_request_bound_to_a_disabled_hookless_task_fails_closed() {
    let harness = Harness::new().await;
    harness.install_task(json!([])).await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    harness
        .apply(vec![(
            Collection::Task,
            json!({
                "node_did": harness.owner(),
                "task_id": TASK_ID,
                "agent_id": harness.agent_config.agent_id,
                "prompt_template": "run the gate",
                "enabled": false,
            }),
        )])
        .await;
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("disabled"), "{row}");
}

#[tokio::test]
async fn a_before_hook_that_outlives_the_request_deadline_fails_the_request_clearly() {
    let harness = Harness::with_deadline(Duration::from_secs(2)).await;
    harness
        .install_task(json!([
            {"hook_id": "prepare", "phase": "before",
             "command": ["sh", "-c", "sleep 3"], "timeout_secs": 30},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.touch("swept")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("request deadline exceeded"), "{row}");
    assert!(harness.mark("swept").exists());
}

/// The owned work's result is observed after its workspace seal, so the
/// after-phase a Task runs is selected by the seal's outcome: after_success
/// cannot gate sealing or integration.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn workspace_seal_completes_before_the_after_phase_is_selected() {
    let harness = Harness::new().await;
    let placement = tempfile::tempdir().expect("workspace placement directory");
    install_workspace(&harness, placement.path()).await;
    harness
        .install_task(json!([
            {"hook_id": "verify", "phase": "after_success",
             "command": ["sh", "-c", harness.touch("verified")], "timeout_secs": 30},
            {"hook_id": "report", "phase": "after_failure",
             "command": ["sh", "-c", harness.touch("reported")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::Trigger, true).await;
    crate::workspace::materialize_workspace_binding(
        harness.node.as_ref(),
        &request.request_id,
        &request.doc_id,
        harness.owner(),
        &crate::lifecycle::WorkspaceLineage {
            workspace_id: request.workspace_id.clone(),
            workspace_authority: request.workspace_authority.clone(),
            workspace_owner_node_did: request.workspace_owner_node_did.clone(),
            workspace_seal_hash: request.workspace_seal_hash.clone(),
        },
    )
    .await
    .expect("materialize the writer binding");
    let row = harness.run(request, false).await;
    assert_eq!(harness.calls(), 1, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("placement"), "the seal failed: {row}");
    assert!(
        harness.mark("reported").exists(),
        "the seal's failure selected after_failure"
    );
    assert!(
        !harness.mark("verified").exists(),
        "after_success never saw the sealed work"
    );
}

impl Harness {
    /// Runs the request and, once `marker` appears, revokes its execution by
    /// installing another generation, as LatestOnly supersession does.
    async fn run_superseding_at(&self, request: AgentRequest, marker: &Path) -> serde_json::Value {
        let doc_id = request.doc_id.clone();
        let mut daemon = self.daemon(false);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let supersede = async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            ConfigAccess::write_local(
                self.node.as_ref(),
                "test.supersede_task_hook_execution",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "replacement-generation" }}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(&doc_id),
                ),
            )
            .await
            .expect("replace the execution generation");
        };
        let (processed, ()) = tokio::time::timeout(Duration::from_secs(25), async {
            tokio::join!(daemon.process_request(request, shutdown_rx), supersede)
        })
        .await
        .expect("revocation cancels the held hook");
        processed.expect("request processing returned");
        self.request_row(&doc_id).await
    }
}

#[tokio::test]
async fn revocation_during_a_before_hook_cancels_it_and_still_cleans_up() {
    let harness = Harness::new().await;
    let held = format!(
        "{}; sleep 30; {}",
        harness.touch("prepare-started"),
        harness.touch("prepare-finished")
    );
    harness
        .install_task(json!([
            {"hook_id": "prepare", "phase": "before",
             "command": ["sh", "-c", held], "timeout_secs": 60},
            {"hook_id": "second", "phase": "before",
             "command": ["sh", "-c", harness.touch("second")], "timeout_secs": 30},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.touch("swept")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let row = harness
        .run_superseding_at(request, &harness.mark("prepare-started"))
        .await;
    assert_eq!(harness.calls(), 0, "{row}");
    assert!(!harness.mark("prepare-finished").exists());
    assert!(
        !harness.mark("second").exists(),
        "no ordinary hook launches after revocation"
    );
    assert!(harness.mark("swept").exists(), "cleanup still runs");
    assert_eq!(
        row["lifecycle_state"], "claimed",
        "the revoking owner keeps the terminal: {row}"
    );
}

#[tokio::test]
async fn revocation_during_an_after_success_hook_cancels_it_and_still_cleans_up() {
    let harness = Harness::new().await;
    let held = format!(
        "{}; sleep 30; {}",
        harness.touch("verify-started"),
        harness.touch("verify-finished")
    );
    harness
        .install_task(json!([
            {"hook_id": "verify", "phase": "after_success",
             "command": ["sh", "-c", held], "timeout_secs": 60},
            {"hook_id": "sweep", "phase": "finally",
             "command": ["sh", "-c", harness.touch("swept")], "timeout_secs": 30},
        ]))
        .await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let row = harness
        .run_superseding_at(request, &harness.mark("verify-started"))
        .await;
    assert_eq!(harness.calls(), 1, "{row}");
    assert!(!harness.mark("verify-finished").exists());
    assert!(harness.mark("swept").exists(), "cleanup still runs");
    assert_eq!(
        row["lifecycle_state"], "processing",
        "the revoking owner keeps the terminal: {row}"
    );
}

#[tokio::test]
async fn revocation_between_hooks_is_checked_before_the_next_renewal_poll() {
    let mut harness = Harness::new().await;
    Arc::get_mut(&mut harness.agent_config)
        .unwrap()
        .stream_liveness_timeout = Duration::from_secs(120);
    let gate = harness.mark("revoked");
    let first = format!(
        "sleep 0.3; {}; while [ ! -e {} ]; do sleep 0.01; done",
        harness.touch("ready"),
        gate.display()
    );
    harness.install_task(json!([
        {"hook_id":"first", "phase":"before", "command":["sh","-c",first], "timeout_secs":10},
        {"hook_id":"second", "phase":"before", "command":["sh","-c",harness.touch("second")], "timeout_secs":10},
        {"hook_id":"cleanup", "phase":"finally", "command":["sh","-c",harness.touch("cleanup")], "timeout_secs":10}
    ])).await;
    let request = harness.create_request(Lineage::ManualFire, false).await;
    let doc_id = request.doc_id.clone();
    let mut daemon = harness.daemon(false);
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let revoke = async {
        while !harness.mark("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        ConfigAccess::write_local(harness.node.as_ref(), "test.revoke_between_task_hooks", &format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "replacement-generation" }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&doc_id)
        )).await.unwrap();
        std::fs::write(gate, "revoked").unwrap();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(daemon.process_request(request, shutdown_rx), revoke)
    })
    .await
    .unwrap();
    result.unwrap();
    let modeled = crate::lean_vocab_test::lean_task_hook_run_cases()
        .iter()
        .find(|case| case.name == "revoked_between_before_hooks_refuses_the_next_launch")
        .unwrap();
    assert!(modeled
        .refused_before_launch
        .iter()
        .any(|id| id == "second"));
    assert_eq!(harness.calls() > 0, modeled.expected_agent_ran);
    assert!(harness.mark("cleanup").exists());
    assert!(
        !harness.mark("second").exists(),
        "second ordinary hook launched AFTER the revocation write committed"
    );
}

#[tokio::test]
async fn natural_finish_excludes_task_and_workspace_success_effects() {
    let harness = Harness::new().await;
    let mut request = harness.create_request(Lineage::ManualFire, false).await;
    request.execution_origin = Some("interactive".into());
    request.input = Default::default();
    request.caused_by_trigger_id = None;
    request.caused_by_source_doc_id = None;
    request.caused_by_parent_tool_call_doc_id = None;
    request.workspace_id = None;
    request.workspace_authority = None;
    assert!(super::permits_natural_finish(&request, false));
    assert!(
        !super::permits_natural_finish(&request, true),
        "a Task without hooks still owns completion"
    );
    request.workspace_id = Some(WORKSPACE_ID.into());
    for authority in ["readWrite", "integrate"] {
        request.workspace_authority = Some(authority.into());
        assert!(
            !super::permits_natural_finish(&request, false),
            "{authority} effects precede terminalization"
        );
    }
    request.workspace_authority = Some("readOnly".into());
    assert!(super::permits_natural_finish(&request, false));
    assert!(!super::permits_natural_finish(&request, true));
}
