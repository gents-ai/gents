use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gents_loop::test_support::scripted_provider::{Reply, ScriptedProvider, Step};
use serde_json::json;

use super::inference::tests::InterruptionCapture;
use super::{AgentDaemon, AgentDaemonInit};
use crate::runtime::agent::runtime::StartupBarrier;
use crate::turn::prompt::LayeredPromptBuilder;
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_inference::resolved::ResolvedAgent;
use gents_loop::tool::ToolDyn;
use gents_protocol::row::AgentRequest;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_toolcalls::call::hook::background_executions::BackgroundExecutionRegistry;
use gents_toolcalls::call::hook::{BackgroundToolRegistry, FailurePolicy};
use tracing::instrument::WithSubscriber;

const TASK_ID: &str = "hook-task";
const TRIGGER_ID: &str = "hook-trigger";
const WORKSPACE_ID: &str = "task-hook-workspace";

/// The agent's provider: it refuses every call when `fail`, never answers
/// when `hang`, and otherwise replies.
fn hook_model(fail: bool, hang: bool) -> ScriptedProvider {
    ScriptedProvider::responding(move |_| match (fail, hang) {
        (true, _) => Reply::provider_error("agent refused"),
        (false, true) => Reply::Stream(vec![Step::Pending]),
        (false, false) => Reply::text("hooked reply"),
    })
    .capturing_openai("scripted")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lineage {
    Trigger,
    ManualFire,
}

fn hook_spec(
    hook_id: &str,
    phase: &str,
    command: impl Into<String>,
    timeout_secs: u64,
) -> serde_json::Value {
    json!({
        "hook_id": hook_id,
        "phase": phase,
        "command": ["sh", "-c", command.into()],
        "timeout_secs": timeout_secs,
    })
}

fn skip_gate_hook(hook_id: &str, exit_code: i32) -> serde_json::Value {
    json!({
        "hook_id": hook_id,
        "phase": "before",
        "skip_exit_codes": [exit_code],
        "command": ["sh", "-c", format!("exit {exit_code}")],
        "timeout_secs": 30,
    })
}

struct TaskHookEnv {
    node: Arc<defra_node::Node>,
    agent_config: Arc<ResolvedAgent>,
    tool_config: Arc<gents_tools::surface::AgentToolSurfaceConfig>,
    models: std::sync::Mutex<Vec<ScriptedProvider>>,
    marks: tempfile::TempDir,
    _db: gents_store::test_support::TestStore,
    _user: gents_store::identity::KeyIdentity,
    user_access: ConfigAccess,
    agent_access: ConfigAccess,
}

impl TaskHookEnv {
    async fn new() -> Self {
        Self::with_deadline(Duration::from_secs(60)).await
    }

    async fn with_deadline(deadline: Duration) -> Self {
        use gents_store::config_client::NodePermission;
        let signed_home = gents_documents::test_support::SignedHome::enroll(
            crate::support::trial_store("task-hooks").await,
            "task-hooks",
        )
        .await;
        let binding = gents_inference::test_support::default_agent(&signed_home).await;
        let (db, user, user_access) = signed_home.into_parts();
        let node = db.node.clone();
        let agent_identity =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let agent_did = identity::Did::new(agent_identity.did()).unwrap();
        let admin = ConfigAccess::local_as(node.clone(), db.node_identity.did().parse().unwrap());
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DocumentDelete,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&agent_did, permission)
                .await
                .unwrap();
        }
        let agent_access =
            ConfigAccess::local_managed_agent(node.clone(), agent_did, agent_identity.as_ref())
                .unwrap();
        let built = crate::runtime::agent::PendingAgent::new("task-hooks")
            .with_identity_binding_for_test(binding)
            .build_with_identity_arc_for_test(db.node_identity.clone());
        let tool_config = built.tools;
        let mut agent_config = built.agent;
        Arc::make_mut(&mut agent_config).deadline_duration = deadline;
        Self {
            node,
            agent_config,
            tool_config,
            models: std::sync::Mutex::default(),
            marks: tempfile::tempdir().expect("hook marker directory"),
            _db: db,
            _user: user,
            user_access,
            agent_access,
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

    fn fifo(&self, name: &str) -> String {
        use std::os::unix::ffi::OsStrExt;
        let path = self.mark(name);
        if !path.exists() {
            let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: `path_c` is a NUL-terminated path valid through mkfifo.
            assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        }
        format!("printf 'ready\\n' > {}", path.display())
    }

    fn held_script(&self, started: &str, finished: &str) -> String {
        format!("{}; sleep 30; {}", self.fifo(started), self.touch(finished))
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
        self.user_access
            .transact_write("test.task_hooks", |txn| {
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
                    "agent_did": self.agent_config.agent_did,
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
        let response = self
            .user_access
            .execute(&format!(
                r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{}" }}, trigger_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(self.owner()),
                gents_store::graphql::escape_graphql_string(TRIGGER_ID),
            ))
            .await
            .unwrap();
        let rows = response["data"]["Trigger"].as_array().unwrap();
        let [row] = rows.as_slice() else {
            panic!("installed Trigger must be unique")
        };
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
                fire_key: gents_automation::triggers::task_fire_key(&identity),
                request_id: identity.request_id(),
                session_id: identity.session_id(),
                identity,
                task_id: TASK_ID.into(),
                goal_id: None,
                goal_objective: None,
                goal_token_budget: None,
                goal_max_infrastructure_retries: None,
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
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                    &self.agent_config.agent_did,
                ),
                "interactive",
            ),
            None => (
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_automated_trigger(
                    &self.agent_config.agent_did,
                    TRIGGER_ID,
                ),
                "scheduled",
            ),
        };
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            request_id,
            gents_protocol::request_admission::RequestScope::new(
                &owner,
                &self.agent_config.agent_did,
                &self.agent_config.agent_did,
                session_id,
            ),
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
        gents_session::request::admission::sign_agent_request_create(
            self.agent_config.agent_identity.as_ref(),
            &mut create,
        )
        .await
        .unwrap();
        let doc_id = match &fire {
            Some(fire) => {
                gents_automation::triggers::write_task_delivery(
                    &self.agent_access,
                    fire,
                    false,
                    &create,
                )
                .await
                .expect("admit the manual Task fire and its request")
                .request
                .doc_id
            }
            None => {
                let response = self
                    .agent_access
                    .write(
                        "test.create_task_hook_request",
                        &create.graphql_mutation().unwrap(),
                    )
                    .await
                    .expect("create task-hook AgentRequest");
                gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap()
            }
        };
        gents_session::request::admission::load_request_for_admission_test_as(
            &self.node,
            &doc_id,
            &self.agent_config.agent_did,
        )
        .await
        .unwrap()
    }

    fn daemon(&self, fail: bool) -> AgentDaemon<ScriptedProvider> {
        self.daemon_model(fail, false, BackgroundExecutionRegistry::default())
    }

    fn daemon_with(
        &self,
        fail: bool,
        executions: BackgroundExecutionRegistry,
    ) -> AgentDaemon<ScriptedProvider> {
        self.daemon_model(fail, false, executions)
    }

    fn daemon_model(
        &self,
        fail: bool,
        hang: bool,
        executions: BackgroundExecutionRegistry,
    ) -> AgentDaemon<ScriptedProvider> {
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &self.agent_config.system_prompt,
            &self.agent_config.agent_did,
            &[],
            false,
            &[],
        );
        let model = hook_model(fail, hang);
        self.models.lock().unwrap().push(model.clone());
        AgentDaemon::new(AgentDaemonInit {
            node: self.node.clone(),
            tool_config: self.tool_config.clone(),
            agent_config: self.agent_config.clone(),
            provider_family: None,
            model: Arc::new(model),
            preamble: prompt_builder.preamble().to_string(),
            loop_tools: Arc::new(Vec::<Box<dyn ToolDyn>>::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    self.node.clone(),
                    self.agent_config.agent_identity.as_ref(),
                )
                .unwrap(),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: executions,
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status: crate::runtime::status::RuntimeStatusHandle::new(
                self.node.clone(),
                self.owner().to_string(),
            ),
            slot_generation: 1,
            request_admission: gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                self.node.clone(),
                self.agent_config.node_identity().clone(),
                gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap()
    }

    async fn request_row(&self, doc_id: &str) -> serde_json::Value {
        let response = self
            .agent_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state failure_reason }} }}"#,
                gents_store::graphql::escape_graphql_string(doc_id),
            ))
            .await
            .unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        let [row] = rows.as_slice() else {
            panic!("held Agent request must be unique")
        };
        row.clone()
    }

    async fn replace_generation(&self, doc_id: &str) {
        self.agent_access
            .write(
                "test.replace_task_hook_generation",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "replacement-generation" }}) {{ _docID }} }}"#,
                    gents_store::graphql::escape_graphql_string(doc_id),
                ),
            )
            .await
            .expect("replace the execution generation");
    }

    async fn run(&self, request: AgentRequest, fail: bool) -> serde_json::Value {
        self.run_with_capacity(request, fail, None).await
    }

    /// Keep the daemon request state machine out of the test future, as the
    /// runtime worker does when it dispatches a request.
    async fn run_with_capacity(
        &self,
        request: AgentRequest,
        fail: bool,
        capacity: Option<crate::runtime::agent::worker_capacity::RequestGuard>,
    ) -> serde_json::Value {
        let doc_id = request.doc_id.clone();
        let mut daemon = self.daemon(fail);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let process =
            Box::pin(daemon.process_request_with_capacity(request, capacity, shutdown_rx));
        tokio::time::timeout(Duration::from_secs(90), process)
            .await
            .expect("request processing finished")
            .expect("request processing returned");
        self.request_row(&doc_id).await
    }

    async fn run_at_fifo<F, Fut>(
        &self,
        request: AgentRequest,
        marker: &Path,
        timeout: Duration,
        on_fifo: F,
    ) -> serde_json::Value
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let doc_id = request.doc_id.clone();
        let mut daemon = self.daemon(false);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let action = async {
            wait_for_fifo(marker).await;
            on_fifo(doc_id.clone()).await;
        };
        let (processed, ()) = tokio::time::timeout(timeout, async {
            tokio::join!(daemon.process_request(request, shutdown_rx), action)
        })
        .await
        .expect("fifo-synchronized request processing finished");
        processed.expect("request processing returned");
        self.request_row(&doc_id).await
    }

    async fn run_interrupting_at(&self, request: AgentRequest, marker: &Path) -> serde_json::Value {
        self.run_at_fifo(
            request,
            marker,
            Duration::from_secs(90),
            |doc_id| async move {
                gents_session::request::interrupt::interrupt_request_by_doc_id_with_access(
                    &self.agent_access,
                    &doc_id,
                    self.owner(),
                    Some(&self.agent_config.agent_did),
                )
                .await
                .expect("interrupt the running request");
            },
        )
        .await
    }

    async fn run_superseding_at(&self, request: AgentRequest, marker: &Path) -> serde_json::Value {
        self.run_at_fifo(
            request,
            marker,
            Duration::from_secs(25),
            |doc_id| async move {
                self.replace_generation(&doc_id).await;
            },
        )
        .await
    }

    async fn crash_at(&self, request: AgentRequest, marker: &Path, records: &Path) {
        let mut daemon = self.daemon_with(
            false,
            BackgroundExecutionRegistry::default().with_task_hook_records(records.to_path_buf()),
        );
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::select! {
            _ = daemon.process_request(request, shutdown_rx) => panic!("the held hook must not finish"),
            () = wait_for_fifo(marker) => {}
        }
    }

    async fn restart(&self, records: &Path, doc_id: &str) {
        let escaped_doc_id = gents_store::graphql::escape_graphql_string(doc_id);
        gents_store::test_support::read_when_changed(
            &self.agent_access,
            "AgentRequest",
            "crashed request relinquishes its execution lease",
            async || {
                let response = self
                    .agent_access
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{ execution_lease_expires_at }} }}"#,
                    ))
                    .await
                    .unwrap();
                let expiry = response["data"]["AgentRequest"][0]["execution_lease_expires_at"]
                    .as_str()
                    .expect("execution_lease_expires_at");
                let deadline = chrono::DateTime::parse_from_rfc3339(expiry).unwrap();
                if deadline < chrono::Utc::now() {
                    Ok(())
                } else {
                    Err(format!("lease expiry {deadline} still >= now"))
                }
            },
        )
        .await;
        let registry =
            BackgroundExecutionRegistry::default().with_task_hook_records(records.to_path_buf());
        let outcome =
            crate::runtime::startup_recovery::run_startup_recovery_with_executions_and_identities(
                &self.node,
                self.owner(),
                std::slice::from_ref(&self.agent_config.agent_identity),
                &registry,
            )
            .await;
        assert!(outcome.task_hooks.expect("task hook recovery") > 0);
        registry.task_hook_records().wait_for_recoveries().await;
        let row = self.request_row(doc_id).await;
        assert!(
            matches!(row["lifecycle_state"].as_str(), Some("failed")),
            "request recovery must decide the abandoned request: {row}"
        );
        assert!(registry.task_hook_records().list().is_empty());
        let again =
            crate::runtime::startup_recovery::run_startup_recovery_with_executions_and_identities(
                &self.node,
                self.owner(),
                std::slice::from_ref(&self.agent_config.agent_identity),
                &registry,
            )
            .await;
        assert!(again.task_hooks.expect("second pass") == 0);
    }

    async fn setup_bound_workspace_request(
        &self,
        hooks: serde_json::Value,
    ) -> (tempfile::TempDir, AgentRequest) {
        let placement = tempfile::tempdir().expect("workspace placement directory");
        install_workspace(self, placement.path()).await;
        self.install_task(hooks).await;
        let request = self.create_request(Lineage::Trigger, true).await;
        materialize_writer_binding(self, &request).await;
        (placement, request)
    }

    /// Provider calls across every daemon this harness built.
    fn calls(&self) -> usize {
        self.models
            .lock()
            .unwrap()
            .iter()
            .map(ScriptedProvider::calls)
            .sum()
    }
}

async fn wait_for_fifo(marker: &Path) {
    use std::os::unix::fs::OpenOptionsExt;

    struct UnblockFifo(PathBuf);
    impl Drop for UnblockFifo {
        fn drop(&mut self) {
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.0);
        }
    }

    let path = marker.to_path_buf();
    let _unblock = UnblockFifo(path.clone());
    tokio::task::spawn_blocking(move || std::fs::read_to_string(&path))
        .await
        .expect("hook marker reader task")
        .expect("read hook started fifo");
}

fn reason(row: &serde_json::Value) -> &str {
    row["failure_reason"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn retirement_leaves_the_same_physical_request_pending_until_a_new_runtime_claims_it() {
    use gents_protocol::serve_lifecycle::replacement::RetirementAuthority;

    let env = TaskHookEnv::new().await;
    env.install_task(json!([hook_spec(
        "prepare",
        "before",
        env.touch("admitted"),
        30
    )]))
    .await;
    let request = env.create_request(Lineage::Trigger, false).await;
    let before = env.request_row(&request.doc_id).await;
    let executions = BackgroundExecutionRegistry::default();
    executions
        .service_admission()
        .retire(RetirementAuthority {
            operator_authenticated: true,
            observed_build_matches: true,
        })
        .unwrap();
    let mut retired = env.daemon_with(false, executions);
    let (_shutdown, receiver) = tokio::sync::watch::channel(false);
    retired
        .process_request(request.clone(), receiver)
        .await
        .unwrap();
    assert_eq!(env.request_row(&request.doc_id).await, before);
    assert_eq!(env.calls(), 0);
    assert!(!env.mark("admitted").exists());

    let row = env.run(request, false).await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(env.calls(), 1);
    assert!(env.mark("admitted").exists());
}

#[tokio::test]
async fn a_failing_before_hook_stops_the_request_before_the_provider() {
    let env = TaskHookEnv::new().await;
    env.install_task(json!([hook_spec(
        "prepare",
        "before",
        "echo blocked >&2; exit 1",
        30
    )]))
    .await;
    let request = env.create_request(Lineage::Trigger, false).await;
    let row = env.run(request, false).await;
    assert_eq!(env.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(
        reason(&row).contains("prepare") && reason(&row).contains("blocked"),
        "{row}"
    );
}

#[tokio::test]
async fn configured_gate_skip_completes_without_provider_or_failure_hooks() {
    let env = TaskHookEnv::new().await;
    env.install_task(json!([
        skip_gate_hook("gate", 10),
        hook_spec("next", "before", env.touch("next"), 30),
        hook_spec("report", "after_failure", env.touch("report"), 30),
        hook_spec("sweep", "finally", env.touch("sweep"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::Trigger, false).await;
    let row = env.run(request, false).await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(env.calls(), 0, "{row}");
    assert!(reason(&row).is_empty(), "{row}");
    assert!(!env.mark("next").exists());
    assert!(!env.mark("report").exists());
    assert!(env.mark("sweep").exists());
}

#[tokio::test]
async fn cleanup_failure_after_a_gate_skip_fails_the_request() {
    let env = TaskHookEnv::new().await;
    env.install_task(json!([
        skip_gate_hook("gate", 10),
        hook_spec("sweep", "finally", "echo cleanup-broke >&2; exit 1", 30),
    ]))
    .await;
    let request = env.create_request(Lineage::Trigger, false).await;
    let row = env.run(request, false).await;
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert_eq!(env.calls(), 0, "{row}");
    assert!(reason(&row).contains("sweep"), "{row}");
    assert!(reason(&row).contains("cleanup-broke"), "{row}");
}

#[tokio::test]
async fn passing_hooks_leave_a_manual_task_run_completed() {
    let env = TaskHookEnv::new().await;
    env.install_task(json!([
        hook_spec("prepare", "before", env.touch("prepared"), 30),
        hook_spec("verify", "after_success", env.touch("verified"), 30),
        hook_spec("sweep", "finally", env.touch("swept"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::ManualFire, false).await;
    let fire_query = format!(
        r#"{{ TriggerFire(filter: {{request_id: {{_eq: "{}"}}}}) {{_docID task_id}} }}"#,
        gents_store::graphql::escape_graphql_string(&request.request_id),
    );
    let selected = env.agent_access.execute(&fire_query).await.unwrap();
    assert_eq!(selected["data"]["TriggerFire"][0]["task_id"], TASK_ID);
    let reader = gents_store::test_support::unrelated_document_reader(&env.node).await;
    let node_view = reader.execute(&fire_query).await;
    assert!(node_view.is_err() || node_view.unwrap()["data"]["TriggerFire"] == json!([]));
    let row = env.run(request, false).await;
    assert_eq!(env.calls(), 1, "{row}");
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    for mark in ["prepared", "verified", "swept"] {
        assert!(env.mark(mark).exists(), "{mark} hook did not run");
    }
}

#[tokio::test]
async fn a_failing_cleanup_keeps_the_agent_failure_as_the_primary_error() {
    let env = TaskHookEnv::new().await;
    env.install_task(json!([
        hook_spec("report", "after_failure", env.touch("reported"), 30),
        hook_spec("sweep", "finally", "exit 1", 30),
    ]))
    .await;
    let request = env.create_request(Lineage::ManualFire, false).await;
    let row = env.run(request, true).await;
    assert!(env.calls() > 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    let reason = reason(&row);
    assert!(!reason.starts_with("task hook"), "{reason:?}");
    assert!(
        reason.contains("task cleanup hooks failed: sweep"),
        "{reason:?}"
    );
    assert!(env.mark("reported").exists());
}

#[tokio::test]
async fn interrupting_a_held_after_success_hook_cancels_it_and_interrupts_the_request() {
    let env = TaskHookEnv::new().await;
    let held = env.held_script("verify-started", "verify-finished");
    env.install_task(json!([
        hook_spec("verify", "after_success", held, 60),
        hook_spec("sweep", "finally", env.touch("swept"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::ManualFire, false).await;
    let capture = InterruptionCapture::default();
    let row = env
        .run_interrupting_at(request, &env.mark("verify-started"))
        .with_subscriber(capture.subscriber())
        .await;
    capture.assert_source("task_hook");
    assert_eq!(env.calls(), 1, "{row}");
    assert_eq!(row["lifecycle_state"], "interrupted", "{row}");
    assert!(!env.mark("verify-finished").exists());
    assert!(
        env.mark("swept").exists(),
        "an interrupt never skips cleanup"
    );
}

#[tokio::test]
async fn interrupting_a_before_hook_skips_the_agent_and_still_cleans_up() {
    let env = TaskHookEnv::new().await;
    let held = env.held_script("prepare-started", "prepare-finished");
    env.install_task(json!([
        hook_spec("prepare", "before", held, 60),
        hook_spec("sweep", "finally", env.touch("swept"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::Trigger, false).await;
    let capture = InterruptionCapture::default();
    let row = env
        .run_interrupting_at(request, &env.mark("prepare-started"))
        .with_subscriber(capture.subscriber())
        .await;
    capture.assert_source("task_hook");
    assert_eq!(env.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "interrupted", "{row}");
    assert!(!env.mark("prepare-finished").exists());
    assert!(env.mark("swept").exists());
}

#[tokio::test]
async fn task_bindings_follow_the_lean_model() {
    use gents_toolcalls::call::task_hooks::TaskBindingRefusal;
    for case in gents_lean_vocab::families::task_hooks_hookless_bound_request::cases::<
        String,
        gents_protocol::request_lifecycle::RequestLifecycleState,
        String,
    >() {
        let env = TaskHookEnv::new().await;
        if case.input.binding != "missing" {
            env.install_task(json!([])).await;
        }
        let request = env.create_request(Lineage::ManualFire, false).await;
        if case.input.binding == "disabled" {
            env.apply(vec![(
                Collection::Task,
                json!({
                    "node_did": env.owner(),
                    "task_id": TASK_ID,
                    "agent_did": env.agent_config.agent_did,
                    "prompt_template": "run the gate",
                    "enabled": false,
                }),
            )])
            .await;
        }
        let row = env.run(request, false).await;
        let refusals = [
            (
                "missing",
                TaskBindingRefusal::Missing {
                    task_id: TASK_ID.into(),
                },
            ),
            (
                "disabled",
                TaskBindingRefusal::Disabled {
                    task_id: TASK_ID.into(),
                },
            ),
        ];
        let refusal = refusals
            .iter()
            .filter(|(_, refusal)| reason(&row).contains(&refusal.to_string()))
            .map(|(name, _)| (*name).to_owned())
            .next();
        case.check(
            gents_lean_vocab::families::task_hooks_hookless_bound_request::BoundRequest {
                agent_ran: env.calls() > 0,
                request_state: serde_json::from_value(row["lifecycle_state"].clone())
                    .expect("persisted lifecycle state"),
                refusal,
            },
        );
    }
}

async fn install_workspace(env: &TaskHookEnv, host_path: &Path) {
    let owner = env.owner().to_string();
    let workspace = gents_session::workspace::IsolatedWorkspaceDoc {
        path_capability: gents_session::workspace::WorkspacePathCapability::exact_paths(vec![
            "patch.rs".to_string(),
        ])
        .unwrap(),
        workspace_id: WORKSPACE_ID.to_string(),
        work_unit_id: TASK_ID.to_string(),
        repository_id: "hook-repo".to_string(),
        base_sha: "0".repeat(40),
        branch: "gents/hook-task".to_string(),
        creation_policy: gents_session::workspace::CreationPolicy::GitWorktreeDiff
            .as_str()
            .to_string(),
        adapter: gents_session::workspace::WorkspaceAdapterKind::GitWorktree
            .as_str()
            .to_string(),
        owner_node_did: owner.clone(),
        writer_principal: env.agent_config.agent_did.clone(),
        integrator_principal: env.agent_config.agent_did.clone(),
        instruction_manifest: String::new(),
        seal_hash: None,
        lifecycle_state: "ready".to_string(),
        caused_by_invocation_id: "hook-invocation".to_string(),
        caused_by_correlation: "hook-correlation".to_string(),
        route: Default::default(),
    };
    let placement = gents_session::workspace::WorkspacePlacementDoc {
        workspace_id: WORKSPACE_ID.to_string(),
        owner_node_did: owner,
        host_path: host_path.to_str().expect("utf-8 path").to_string(),
        repository_placement_id: "hook-repo".to_string(),
        adapter: gents_session::workspace::WorkspaceAdapterKind::GitWorktree
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
        gents_session::workspace::isolated_workspace_upsert_mutation(&workspace),
        gents_session::workspace::workspace_placement_upsert_mutation(&placement, &updated_at),
    ] {
        env.agent_access
            .write("test.task_hook_workspace", &mutation)
            .await
            .expect("install isolated workspace and placement");
    }
}

async fn workspace_bindings(
    env: &TaskHookEnv,
) -> Vec<gents_session::workspace::WorkspaceBindingDoc> {
    let response = env
        .agent_access
        .execute(&format!(
            r#"{{ WorkspaceBinding(filter: {{ workspace_id: {{ _eq: "{}" }}, owner_node_did: {{ _eq: "{}" }} }}) {{ binding_id workspace_id request_id request_doc_id authority owner_node_did seal_hash lifecycle_state }} }}"#,
            gents_store::graphql::escape_graphql_string(WORKSPACE_ID),
            gents_store::graphql::escape_graphql_string(env.owner()),
        ))
        .await
        .unwrap();
    let response: query::QueryResponse = serde_json::from_value(response).unwrap();
    gents_store::graphql::rows(&response, "WorkspaceBinding").unwrap()
}

async fn materialize_writer_binding(env: &TaskHookEnv, request: &AgentRequest) {
    gents_session::workspace::materialize_workspace_binding(
        &env.node,
        &request.request_id,
        &request.doc_id,
        env.agent_config.agent_identity.as_ref(),
        &gents_session::request::lifecycle::WorkspaceLineage {
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
    let env = TaskHookEnv::new().await;
    let (_placement, request) = env.setup_bound_workspace_request(json!([])).await;
    assert!(workspace_bindings(&env)
        .await
        .iter()
        .any(|binding| binding.is_active_read_write()));
    env.agent_access
        .write(
            "test.remove_workspace_placement",
            &format!(
                r#"mutation {{ delete_WorkspacePlacement(filter: {{ workspace_id: {{ _eq: "{}" }}, owner_node_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(WORKSPACE_ID),
                gents_store::graphql::escape_graphql_string(env.owner()),
            ),
        )
        .await
        .unwrap();
    let row = env.run(request, false).await;
    assert_eq!(env.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("workspace"), "{row}");
    assert!(workspace_bindings(&env)
        .await
        .iter()
        .all(|binding| !binding.is_active()));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn refused_worker_ticket_releases_the_bound_workspace_before_work() {
    use crate::runtime::agent::worker_capacity::{RequestGuard, WorkerCapacity, WorkerTicket};

    let env = TaskHookEnv::new().await;
    let (_placement, request) = env.setup_bound_workspace_request(json!([])).await;
    let mut guard = Some(RequestGuard::Unbound(
        WorkerCapacity::new(1).try_acquire_unbound().unwrap(),
    ));
    RequestGuard::bind_claim(
        &mut guard,
        WorkerTicket::new("previous-request", "previous-generation"),
    )
    .unwrap();
    let row = env.run_with_capacity(request, false, guard).await;
    assert_eq!(env.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("worker guard"), "{row}");
    assert!(workspace_bindings(&env)
        .await
        .iter()
        .all(|binding| !binding.is_active()));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_skipped_gate_releases_the_bound_workspace() {
    let env = TaskHookEnv::new().await;
    let (_placement, request) = env
        .setup_bound_workspace_request(json!([skip_gate_hook("gate", 10)]))
        .await;
    assert!(workspace_bindings(&env)
        .await
        .iter()
        .any(gents_session::workspace::WorkspaceBindingDoc::is_active_read_write));
    let row = env.run(request, false).await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(env.calls(), 0, "{row}");
    assert!(workspace_bindings(&env)
        .await
        .iter()
        .all(|binding| !binding.is_active()));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_failing_before_hook_releases_the_bound_workspace() {
    let env = TaskHookEnv::new().await;
    let (_placement, request) = env
        .setup_bound_workspace_request(json!([hook_spec("prepare", "before", "exit 1", 30)]))
        .await;
    assert!(workspace_bindings(&env)
        .await
        .iter()
        .any(gents_session::workspace::WorkspaceBindingDoc::is_active_read_write));
    let row = env.run(request, false).await;
    assert_eq!(env.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("prepare"), "{row}");
    let bindings = workspace_bindings(&env).await;
    assert!(
        !bindings
            .iter()
            .any(gents_session::workspace::WorkspaceBindingDoc::is_active_read_write),
        "{bindings:?}"
    );
}

#[tokio::test]
async fn a_restart_during_a_before_hook_runs_cleanup_once_without_rerunning_it() {
    let env = TaskHookEnv::new().await;
    let held = env.logging(
        "prepare",
        &env.held_script("prepare-started", "prepare-finished"),
    );
    env.install_task(json!([
        hook_spec("prepare", "before", held, 60),
        hook_spec("sweep", "finally", env.logging("sweep", "true"), 30),
    ]))
    .await;
    let records = env.mark("task-hooks");
    let request = env.create_request(Lineage::ManualFire, false).await;
    let doc_id = request.doc_id.clone();
    env.crash_at(request, &env.mark("prepare-started"), &records)
        .await;
    env.restart(&records, &doc_id).await;
    assert_eq!(env.log_lines(), vec!["prepare", "sweep"]);
    assert!(!env.mark("prepare-finished").exists());
    assert_eq!(env.calls(), 0);
}

#[tokio::test]
async fn a_restart_during_cleanup_runs_only_the_remaining_cleanup() {
    let env = TaskHookEnv::new().await;
    let held = env.logging("first", &format!("{}; sleep 30", env.fifo("first-started")));
    env.install_task(json!([
        hook_spec("first", "finally", held, 60),
        hook_spec("second", "finally", env.logging("second", "true"), 30),
    ]))
    .await;
    let records = env.mark("task-hooks");
    let request = env.create_request(Lineage::Trigger, false).await;
    let doc_id = request.doc_id.clone();
    env.crash_at(request, &env.mark("first-started"), &records)
        .await;
    env.restart(&records, &doc_id).await;
    assert_eq!(env.log_lines(), vec!["first", "second"]);
}

#[tokio::test]
async fn losing_the_lease_mid_work_skips_after_failure_and_still_cleans_up() {
    let env = TaskHookEnv::new().await;
    env.install_task(json!([
        hook_spec("report", "after_failure", env.touch("reported"), 30),
        hook_spec("sweep", "finally", env.touch("swept"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::ManualFire, false).await;
    let doc_id = request.doc_id.clone();
    let mut daemon = env.daemon_model(false, true, BackgroundExecutionRegistry::default());
    let model = env
        .models
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("daemon model");
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let replace = async {
        model.wait_for_calls(1).await;
        env.replace_generation(&doc_id).await;
    };
    let (processed, ()) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(daemon.process_request(request, shutdown_rx), replace)
    })
    .await
    .expect("lease loss is observed");
    processed.expect("request processing returned");
    let row = env.request_row(&doc_id).await;
    assert!(
        !env.mark("reported").exists(),
        "a lost lease is not this execution's failure: {row}"
    );
    assert!(env.mark("swept").exists(), "cleanup still runs live");
    assert_eq!(
        row["lifecycle_state"], "processing",
        "the current owner keeps the terminal: {row}"
    );
}

#[tokio::test]
async fn shutdown_before_any_hook_launches_leaves_no_record_to_recover() {
    let env = TaskHookEnv::new().await;
    env.install_task(json!([
        hook_spec("prepare", "before", env.logging("prepare", "true"), 30),
        hook_spec("sweep", "finally", env.logging("sweep", "true"), 30),
    ]))
    .await;
    let records = env.mark("task-hooks");
    let executions = BackgroundExecutionRegistry::default().with_task_hook_records(records.clone());
    let request = env.create_request(Lineage::ManualFire, false).await;
    let mut daemon = env.daemon_model(false, false, executions.clone());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(true);
    daemon
        .process_request(request, shutdown_rx)
        .await
        .expect("request processing returned");
    assert!(env.log_lines().is_empty(), "no hook launched");
    assert!(env.calls() == 0);
    assert!(
        executions.task_hook_records().list().is_empty(),
        "an execution that never started leaves nothing for recovery"
    );
    let restarted = BackgroundExecutionRegistry::default().with_task_hook_records(records);
    let outcome =
        crate::runtime::startup_recovery::run_startup_recovery_with_executions_and_identities(
            &env.node,
            env.owner(),
            std::slice::from_ref(&env.agent_config.agent_identity),
            &restarted,
        )
        .await;
    assert!(outcome.task_hooks.unwrap() == 0);
}

#[tokio::test]
async fn a_before_hook_that_outlives_the_request_deadline_fails_the_request_clearly() {
    let env = TaskHookEnv::with_deadline(Duration::from_secs(2)).await;
    env.install_task(json!([
        hook_spec("prepare", "before", "sleep 3", 30),
        hook_spec("sweep", "finally", env.touch("swept"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::ManualFire, false).await;
    let row = env.run(request, false).await;
    assert_eq!(env.calls(), 0, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("request deadline exceeded"), "{row}");
    assert!(env.mark("swept").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn workspace_seal_completes_before_the_after_phase_is_selected() {
    let env = TaskHookEnv::new().await;
    let (_placement, request) = env
        .setup_bound_workspace_request(json!([
            hook_spec("verify", "after_success", env.touch("verified"), 30),
            hook_spec("report", "after_failure", env.touch("reported"), 30),
        ]))
        .await;
    let row = env.run(request, false).await;
    assert_eq!(env.calls(), 1, "{row}");
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert!(reason(&row).contains("placement"), "the seal failed: {row}");
    assert!(
        env.mark("reported").exists(),
        "the seal's failure selected after_failure"
    );
    assert!(
        !env.mark("verified").exists(),
        "after_success never saw the sealed work"
    );
}

#[tokio::test]
async fn revocation_during_a_before_hook_cancels_it_and_still_cleans_up() {
    let mut env = TaskHookEnv::new().await;
    // A revocation writes no interrupt latch; the execution observes it at its
    // next lease renewal poll (a quarter of the lease), inside the 25s bound.
    Arc::get_mut(&mut env.agent_config)
        .unwrap()
        .stream_liveness_timeout = Duration::from_secs(8);
    let held = env.held_script("prepare-started", "prepare-finished");
    env.install_task(json!([
        hook_spec("prepare", "before", held, 60),
        hook_spec("second", "before", env.touch("second"), 30),
        hook_spec("sweep", "finally", env.touch("swept"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::ManualFire, false).await;
    let row = env
        .run_superseding_at(request, &env.mark("prepare-started"))
        .await;
    assert_eq!(env.calls(), 0, "{row}");
    assert!(!env.mark("prepare-finished").exists());
    assert!(
        !env.mark("second").exists(),
        "no ordinary hook launches after revocation"
    );
    assert!(env.mark("swept").exists(), "cleanup still runs");
    assert_eq!(
        row["lifecycle_state"], "claimed",
        "the revoking owner keeps the terminal: {row}"
    );
}

#[tokio::test]
async fn revocation_during_an_after_success_hook_cancels_it_and_still_cleans_up() {
    let mut env = TaskHookEnv::new().await;
    // A revocation writes no interrupt latch; the execution observes it at its
    // next lease renewal poll (a quarter of the lease), inside the 25s bound.
    Arc::get_mut(&mut env.agent_config)
        .unwrap()
        .stream_liveness_timeout = Duration::from_secs(8);
    let held = env.held_script("verify-started", "verify-finished");
    env.install_task(json!([
        hook_spec("verify", "after_success", held, 60),
        hook_spec("sweep", "finally", env.touch("swept"), 30),
    ]))
    .await;
    let request = env.create_request(Lineage::ManualFire, false).await;
    let row = env
        .run_superseding_at(request, &env.mark("verify-started"))
        .await;
    assert_eq!(env.calls(), 1, "{row}");
    assert!(!env.mark("verify-finished").exists());
    assert!(env.mark("swept").exists(), "cleanup still runs");
    assert_eq!(
        row["lifecycle_state"], "processing",
        "the revoking owner keeps the terminal: {row}"
    );
}

#[tokio::test]
async fn revocation_between_hooks_is_checked_before_the_next_renewal_poll() {
    for case in gents_lean_vocab::families::task_hooks_contracts_daemon_revocation::cases::<
        String,
        String,
        gents_documents::TaskHookPhase,
    >() {
        let mut env = TaskHookEnv::new().await;
        Arc::get_mut(&mut env.agent_config)
            .unwrap()
            .stream_liveness_timeout = Duration::from_secs(120);
        let gate = env.mark("revoked");
        let gated = case
            .input
            .hooks
            .iter()
            .find(|hook| {
                hook.phase == gents_documents::TaskHookPhase::Before
                    && !case.input.revoked.contains(&hook.hook_id)
            })
            .map(|hook| hook.hook_id.clone())
            .expect("the scenario revokes after an owned before hook");
        let hooks = case
            .input
            .hooks
            .iter()
            .map(|hook| {
                let command = if hook.hook_id == gated {
                    format!(
                        "sleep 0.3; {}; {}; while [ ! -e {} ]; do sleep 0.01; done",
                        env.touch(&hook.hook_id),
                        env.fifo("ready"),
                        gate.display()
                    )
                } else {
                    env.touch(&hook.hook_id)
                };
                json!({
                    "hook_id": hook.hook_id, "phase": hook.phase,
                    "command": ["sh", "-c", command], "timeout_secs": 10
                })
            })
            .collect::<Vec<_>>();
        env.install_task(json!(hooks)).await;
        let request = env.create_request(Lineage::ManualFire, false).await;
        let env_ref = &env;
        env.run_at_fifo(
            request,
            &env.mark("ready"),
            Duration::from_secs(15),
            |doc_id| async move {
                env_ref.replace_generation(&doc_id).await;
                std::fs::write(&gate, "revoked").unwrap();
            },
        )
        .await;
        let refused_before_launch = case
            .input
            .hooks
            .iter()
            .filter(|hook| !env.mark(&hook.hook_id).exists())
            .map(|hook| hook.hook_id.clone())
            .collect();
        case.check(
            gents_lean_vocab::families::task_hooks_contracts_daemon_revocation::DaemonOutput {
                refused_before_launch,
                agent_ran: env.calls() > 0,
            },
        );
    }
}

#[tokio::test]
async fn natural_finish_excludes_task_and_workspace_success_effects() {
    let env = TaskHookEnv::new().await;
    let mut request = env.create_request(Lineage::ManualFire, false).await;
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
