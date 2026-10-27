use super::*;
use crate::identity::NodeIdentity;
use crate::pack::{write_home_install, HomePackInstall, InstalledVia, PackKind};
use crate::plugin::executor::PluginExecutor;
use crate::plugin::store;
use crate::plugin::tests::executor::installed_plugin;
use crate::plugin_job::driver::{settle_plugin_job, JobDriver, JobRuntime, Settlement};
use crate::plugin_job::input::tests::{insert, live_settings};
use crate::plugin_job::keys::{job_session_id, job_spawn_key};
use crate::plugin_job::records::{read_pin, read_progress, read_step_state, write_pin_in_txn};
use crate::plugin_job::JobBinding;
use crate::tool_call_lifecycle::admission_fixture::{fixture_job_pin, write_fixture_job_pin};
use gents_protocol::job::{
    AssetMountDeclaration, DynamicMode, JobPin, JobPolicy, JobState, PluginDataDeclaration,
    PluginJobDeclaration, PluginJobSettings,
};
use serde_json::Value;

const TOOL: &str = "workflow";
const PIN: &str =
    r#""pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 2, "handle": "h"}"#;
const SPAWN: &str =
    r#"{"slot": "s", "attempt": 0, "target": "t-worker", "prompt": "go", "title": "t"}"#;

struct Options {
    settings: Option<PluginJobSettings>,
    via: InstalledVia,
    driver: bool,
    data: bool,
    /// The calling request's hop; a request is started by a job at hop 1 or more.
    hop: u32,
    /// Runs the driver's `tool_root` in a temporary working folder.
    cwd: bool,
    /// Later steps of an adopted job end without effect and leave it running,
    /// instead of failing on a refused scratch file.
    idle: bool,
}

/// The job block of `team/plugin` in the caller's Tools: it may start `worker`.
fn worker_settings(policy: JobPolicy) -> PluginJobSettings {
    let mut settings = live_settings(policy);
    settings.agent_targets = vec!["t-worker".into()];
    settings
}

fn running_policy() -> JobPolicy {
    JobPolicy {
        dynamic: DynamicMode::Run,
        dynamic_targets: vec!["t-worker".into()],
        ..Default::default()
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            settings: Some(worker_settings(running_policy())),
            via: InstalledVia::Operator,
            driver: true,
            data: false,
            hop: 0,
            cwd: false,
            idle: false,
        }
    }
}

struct Step0 {
    _dir: tempfile::TempDir,
    home: tempfile::TempDir,
    cwd: Option<tempfile::TempDir>,
    node: Arc<EmbeddedNode>,
    hook: DefraSessionHook,
    request_id: String,
    session_id: String,
    data: bool,
    via: InstalledVia,
    idle: bool,
}

/// A guest that answers step 0 with `first` and every later step (its input
/// carries a `"pin":{` key) with a scratch file whose name the driver refuses,
/// so the step the adoption kicks leaves the row untouched.
fn branching_wat(first: &[u8], later: &[u8]) -> String {
    let escape = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 0) "{first_bytes}")
  (data (i32.const 4096) "{later_bytes}")
  (func (export "_start")
    (local $i i32) (local $end i32) (local $later i32)
    (i32.store (i32.const 8208) (i32.const 16384))
    (i32.store (i32.const 8212) (i32.const 32768))
    (drop (call $fd_read (i32.const 0) (i32.const 8208) (i32.const 1) (i32.const 8216)))
    (local.set $i (i32.const 16384))
    (local.set $end (i32.add (i32.const 16384) (i32.load (i32.const 8216))))
    (block $done
      (loop $scan
        (br_if $done (i32.gt_u (i32.add (local.get $i) (i32.const 7)) (local.get $end)))
        (if (i32.and
              (i32.eq (i32.load (local.get $i)) (i32.const 0x6e697022))
              (i32.and
                (i32.eq (i32.load8_u offset=4 (local.get $i)) (i32.const 0x22))
                (i32.and
                  (i32.eq (i32.load8_u offset=5 (local.get $i)) (i32.const 0x3a))
                  (i32.eq (i32.load8_u offset=6 (local.get $i)) (i32.const 0x7b)))))
          (then (local.set $later (i32.const 1)) (br $done)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $scan)))
    (i32.store (i32.const 8192) (select (i32.const 4096) (i32.const 0) (local.get $later)))
    (i32.store (i32.const 8196) (select (i32.const {later_len}) (i32.const {first_len}) (local.get $later)))
    (drop (call $fd_write (i32.const 1) (i32.const 8192) (i32.const 1) (i32.const 8200)))))
"#,
        first_bytes = escape(first),
        later_bytes = escape(later),
        first_len = first.len(),
        later_len = later.len(),
    )
}

/// Installs `team/plugin` under `home`, answering step 0 with `output`,
/// replacing any earlier install of it.
fn install_plugin(
    home: &std::path::Path,
    output: &Value,
    data: bool,
    via: InstalledVia,
    idle: bool,
) {
    let later = if idle {
        json!({"effects": {}})
    } else {
        json!({"result": "x", "scratch": [{"name": "a/b", "content": "c"}]})
    };
    let wat = branching_wat(output.to_string().as_bytes(), later.to_string().as_bytes());
    let (source, mut record) = installed_plugin(&wat, None);
    let hex = record.digest.strip_prefix("sha256:").unwrap().to_owned();
    store::store_bytes(home, &hex, &store::read_bytes(source.path(), &hex).unwrap()).unwrap();
    record.declaration.job = PluginJobDeclaration(true);
    record.declaration.asset_mounts = vec![AssetMountDeclaration {
        class: "workflows".into(),
    }];
    record.owner_pack_coordinate = Some("team/plugin".into());
    if data {
        record.declaration.data = Some(PluginDataDeclaration {
            collections: vec!["Plain".into()],
            ops: vec!["insert".into()],
            stamped: Default::default(),
        });
        record.data_collection_fields = [("Plain".to_owned(), vec!["body".to_owned()])].into();
    }
    store::write_record(home, &record).unwrap();
    write_home_install(
        home,
        &HomePackInstall {
            coordinate: "team/plugin".into(),
            version: "1.0.0".into(),
            digest: record.digest.clone(),
            kind: PackKind::Plugins,
            assets: String::new(),
            dependency_assets: vec![],
            plugins: vec![],
            ux: Vec::new(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            installed_via: Some(via),
        },
    )
    .unwrap();
}

/// A hook bound to the job tool `workflow`, whose installed plugin answers
/// every step 0 with `output`.
async fn step0(output: Value, options: Options) -> Step0 {
    let dir = tempfile::tempdir().unwrap();
    let identity =
        crate::identity::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node.add_collection("type Plain { body: String }")
        .await
        .unwrap();
    crate::test_support::install_test_agent(&node, identity.did(), "general").await;
    crate::test_support::install_test_agent(&node, identity.did(), "worker").await;
    insert(
        &node,
        "AgentTarget",
        json!({"target_id": "t-worker", "node_did": identity.did(), "target_node_did": identity.did(),
               "agent_id": "worker", "name": "worker"}),
    )
    .await;
    let tools: crate::document_config::Tools = serde_json::from_value(json!({
        "tools_id": "general:tools",
        "node_did": identity.did(),
        "agents": {"enabled": true, "target_ids": ["t-worker"]},
        "integrations": {"plugins": [{"plugin": "team/plugin", "job": options.settings}]},
    }))
    .unwrap();
    crate::config_client::write_tools_document(
        &crate::config_client::ConfigAccess::local(node.clone()),
        &tools,
    )
    .await
    .unwrap();

    let home = tempfile::tempdir().unwrap();
    install_plugin(
        home.path(),
        &output,
        options.data,
        options.via,
        options.idle,
    );
    let cwd = options.cwd.then(|| tempfile::tempdir().unwrap());

    let binding = JobBinding {
        tool_name: TOOL.into(),
        plugin: serde_json::from_value(json!({"plugin": "team/plugin"})).unwrap(),
        settings: options.settings,
    };
    let mut hook =
        DefraSessionHook::with_identity(node.clone(), identity.did(), FailurePolicy::default())
            .with_job_tools(RapidHashMap::from_iter([(TOOL.to_owned(), binding)]));
    if options.driver {
        hook = hook.with_job_driver(JobDriver::new(JobRuntime {
            node: node.clone(),
            plugins: Arc::new(PluginExecutor::new(Some(home.path().to_owned()))),
            home: home.path().to_owned(),
            tool_root: cwd.as_ref().map(|cwd| cwd.path().to_owned()),
        }));
    }
    let session_id = hook.session_id().await.unwrap();
    let request_id = format!("step0-{}", uuid::Uuid::new_v4());
    let cause = match options.hop {
        0 => None,
        depth => Some(job_ancestry(&node, depth).await),
    };
    bind_request_at_hop(&node, &hook, &request_id, &session_id, options.hop, cause).await;
    hook.on_completion_call(&user_text_message("run the workflow"), &[])
        .await;
    Step0 {
        _dir: dir,
        home,
        cwd,
        node,
        hook,
        request_id,
        session_id,
        data: options.data,
        via: options.via,
        idle: options.idle,
    }
}

async fn bind_request_at_hop(
    node: &Arc<EmbeddedNode>,
    hook: &DefraSessionHook,
    request_id: &str,
    session_id: &str,
    hop: u32,
    cause: Option<(String, String)>,
) {
    let identity = crate::identity::RegisteredIdentity::from_registered_did(&hook.node_did, None)
        .expect("registered job fixture identity");
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        identity.did(),
        identity.did(),
        "general",
        session_id,
        "run",
        "interactive",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    create.request_hop = hop;
    if let Some((request_doc_id, call_doc_id)) = cause {
        let response = crate::ConfigAccess::local(node.clone()).execute(&format!(
            r#"{{AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}) {{request_id}} AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}) {{tool_call_id}}}}"#,
            crate::graphql::escape_graphql_string(&request_doc_id),
            crate::graphql::escape_graphql_string(&call_doc_id),
        )).await.unwrap();
        create.caused_by_parent_request_id = Some(
            response["data"]["AgentRequest"][0]["request_id"]
                .as_str()
                .unwrap()
                .into(),
        );
        create.caused_by_parent_request_doc_id = Some(request_doc_id);
        create.caused_by_parent_tool_call_id = Some(
            response["data"]["AgentToolCall"][0]["tool_call_id"]
                .as_str()
                .unwrap()
                .into(),
        );
        create.caused_by_parent_tool_call_doc_id = Some(call_doc_id);
    }
    let lifecycle = crate::tool_call_lifecycle::admission_fixture::claimed_signed_request_create(
        node, &identity, create,
    )
    .await;
    bind_claimed_hook_request(hook, lifecycle, Utc::now() + chrono::Duration::minutes(5)).await;
}

/// The (request, tool call) documents the calling request is caused by: a
/// job `depth` sessions up, through `depth - 1` plain tool calls.
async fn job_ancestry(node: &Arc<EmbeddedNode>, depth: u32) -> (String, String) {
    let request = |id: &str, session: &str, parent: Option<(&str, &str)>| {
        let mut value = json!({
            "request_id": id,
            "purpose": "normal",
            "node_did": "owner",
            "requester_did": null,
            "session_id": session,
            "agent_id": "configured",
            "lifecycle_state": "processing",
        });
        if let Some((request_doc_id, call_doc_id)) = parent {
            value["caused_by_parent_request_id"] = json!("parent");
            value["caused_by_parent_request_doc_id"] = json!(request_doc_id);
            value["caused_by_parent_tool_call_id"] = json!("call");
            value["caused_by_parent_tool_call_doc_id"] = json!(call_doc_id);
        }
        value
    };
    let tool = |id: &str, session: &str, request_doc_id: &str| {
        json!({
            "request_id": session,
            "request_doc_id": request_doc_id,
            "node_did": "owner",
            "requester_did": null,
            "session_id": session,
            "tool_call_id": id,
            "tool_name": "agent_new",
            "lifecycle_state": "running",
            "await_mode": "background",
        })
    };
    let root = insert(node, "AgentRequest", request("root", "root", None)).await;
    let job = insert(node, "AgentToolCall", tool("job", "root", &root)).await;
    write_fixture_job_pin(node, &job).await.unwrap();
    let (mut parent, mut cause) = (root, job);
    for level in 1..depth {
        let session = format!("level-{level}");
        let mid = insert(
            node,
            "AgentRequest",
            request(&session, &session, Some((&parent, &cause))),
        )
        .await;
        cause = insert(node, "AgentToolCall", tool(&session, &session, &mid)).await;
        parent = mid;
    }
    (parent, cause)
}

impl Step0 {
    /// Calls the job tool and returns the result the model reads.
    async fn call(&self, args: &str) -> Value {
        self.call_as("call-1", args).await
    }

    async fn call_as(&self, call_id: &str, args: &str) -> Value {
        accept_hook_tool_call(&self.hook, call_id, TOOL, args, None).await;
        let action = self.hook.on_tool_call(TOOL, None, call_id, args).await;
        let ToolCallHookAction::Skip { reason } = action else {
            panic!("a job tool call is answered by its hook: {action:?}");
        };
        serde_json::from_str(&reason).unwrap_or(Value::String(reason))
    }

    async fn row(&self) -> Value {
        self.row_of("call-1").await
    }

    async fn row_of(&self, call_id: &str) -> Value {
        let response = self
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ request_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{TOOL}" }}, tool_call_id: {{ _eq: "{call_id}" }} }}) {{ _docID lifecycle_state tool_failure_class await_mode job_pin job_progress plugin_execution_receipt }} }}"#,
                self.request_id
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let rows = response.data.unwrap()["AgentToolCall"].clone();
        assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
        rows[0].clone()
    }

    fn access(&self) -> crate::config_client::ConfigAccess {
        crate::config_client::ConfigAccess::local(self.node.clone())
    }

    async fn pin_of(&self, row: &Value) -> JobPin {
        read_pin(&self.access(), row["_docID"].as_str().unwrap())
            .await
            .unwrap()
            .expect("an adopted row holds its job_pin")
    }

    /// The invocation reply stored for the row, as the model reads it.
    async fn stored_reply(&self, row: &Value) -> Value {
        let message = crate::tool_call_lifecycle::query::load_tool_call_result(
            &self.access(),
            row["_docID"].as_str().unwrap(),
            &self.hook.node_did,
            &self.session_id,
            Some(&self.hook.node_did),
        )
        .await
        .unwrap();
        serde_json::from_str(
            &crate::tool_call_lifecycle::query::render_tool_result(&message).unwrap(),
        )
        .unwrap()
    }

    /// Replaces the installed plugin, as an operator reinstall does.
    fn reinstall(&self, output: &Value) {
        install_plugin(self.home.path(), output, self.data, self.via, self.idle);
    }

    /// The requests whose retry key names a spawn of the job rooted at `root`.
    async fn child_request(&self, root: &str, slot: &str) -> Value {
        let response = self
            .node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{ _docID request_id session_id agent_id }} }}"#,
                job_spawn_key(root, slot, 0)
            ))
            .await;
        let rows = response.data.unwrap()["AgentRequest"].clone();
        assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
        rows[0].clone()
    }

    async fn bodies(&self) -> Vec<String> {
        let response = self.node.execute("{ Plain { body } }").await;
        response.data.unwrap()["Plain"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["body"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn finish(self) {
        self.node.shutdown().await;
    }
}

fn failed(row: &Value, class: &str) {
    assert_eq!(row["lifecycle_state"], "failed", "{row}");
    assert_eq!(row["tool_failure_class"], class, "{row}");
    assert!(row["job_pin"].is_null(), "{row}");
}

#[tokio::test]
async fn named_run_source_boundaries_follow_the_model_and_preserve_accepted_arguments() {
    let step = step0(
        json!({"result": {"answer": 42}}),
        Options {
            settings: Some(worker_settings(JobPolicy {
                dynamic: DynamicMode::Off,
                ..running_policy()
            })),
            ..Default::default()
        },
    )
    .await;
    for (index, name) in [
        "absent_sources_under_off",
        "empty_script_under_off",
        "empty_path_under_off",
        "both_empty_under_off",
        "nonempty_script_under_off",
        "nonempty_path_under_off",
        "whitespace_script_is_source",
        "whitespace_path_is_source",
    ]
    .into_iter()
    .enumerate()
    {
        let case = crate::lean_vocab_test::lean_plugin_job_admission_cases()
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let arguments = json!({
            "argv": case.job.call.argv,
            "script": case.job.call.script,
            "path": case.job.call.path,
        });
        let call = format!("source-boundary-{index}");
        let answer = step.call_as(&call, &arguments.to_string()).await;
        let row = step.row_of(&call).await;
        if case.expected.admitted {
            assert_eq!(answer, json!({"answer": 42}), "{name}");
            assert_eq!(row["lifecycle_state"], "completed", "{name}: {row}");
        } else {
            assert!(case.expected.dynamic_code_disabled, "{name}");
            assert_eq!(
                answer["code"].as_str(),
                case.expected.code.as_deref(),
                "{name}"
            );
            assert!(
                answer["message"]
                    .as_str()
                    .unwrap()
                    .contains("job.policy.dynamic"),
                "{name}: {answer}"
            );
            assert!(
                answer["next"]
                    .as_str()
                    .unwrap()
                    .contains("operator-installed script"),
                "{name}: {answer}"
            );
            failed(&row, "policyDenied");
        }
        let accepted = crate::tool_call_lifecycle::load_tool_call_arguments(
            &step.access(),
            row["_docID"].as_str().unwrap(),
            &step.hook.node_did,
            &step.session_id,
            Some(&step.hook.node_did),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&accepted).unwrap(),
            arguments,
            "{name}"
        );
    }
    step.finish().await;
}

#[tokio::test]
async fn non_job_result_with_writes_commits_atomically_with_the_foreground_terminal() {
    let step = step0(
        json!({
            "result": {"answer": 42},
            "writes": [{"op": "insert", "collection": "Plain", "fields": {"body": "x"}}],
        }),
        Options {
            data: true,
            ..Default::default()
        },
    )
    .await;
    let answer = step.call(r#"{"argv": ["status"]}"#).await;
    assert_eq!(answer, json!({"answer": 42}));
    let row = step.row().await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(row["await_mode"], "foreground", "{row}");
    assert!(row["job_pin"].is_null(), "{row}");
    assert!(!row["plugin_execution_receipt"].is_null(), "{row}");
    assert_eq!(step.bodies().await, ["x"]);
    step.finish().await;
}

#[tokio::test]
async fn pinless_effects_are_refused_with_policy_denied() {
    let step = step0(json!({"effects": {}}), Options::default()).await;
    let answer = step.call(r#"{"argv": ["run"]}"#).await;
    assert_eq!(answer["code"], "plugin_job_policy_denied", "{answer}");
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("step 0 effects require a pin"),
        "{answer}"
    );
    failed(&step.row().await, "policyDenied");
    step.finish().await;
}

#[tokio::test]
async fn pinless_scratch_is_refused() {
    let step = step0(
        json!({"result": "done", "scratch": [{"name": "a.txt", "content": "x"}]}),
        Options::default(),
    )
    .await;
    let answer = step.call(r#"{"argv": ["run"]}"#).await;
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("step 0 scratch requires a pin"),
        "{answer}"
    );
    failed(&step.row().await, "policyDenied");
    step.finish().await;
}

#[tokio::test]
async fn nested_job_is_refused_at_depth_1_and_2() {
    for depth in [1, 2] {
        let step = step0(
            pinned_result(),
            Options {
                hop: depth,
                ..Default::default()
            },
        )
        .await;
        let answer = step.call(r#"{"argv": ["run"]}"#).await;
        assert_eq!(
            answer["code"], "plugin_job_depth_exceeded",
            "depth {depth}: {answer}"
        );
        failed(&step.row().await, "policyDenied");
        step.finish().await;
    }
}

#[tokio::test]
async fn pin_under_the_empty_ceiling_is_not_available_with_the_next_call() {
    let step = step0(
        pinned_result(),
        Options {
            settings: None,
            ..Default::default()
        },
    )
    .await;
    let answer = step.call(r#"{"argv": ["run"]}"#).await;
    assert_eq!(answer["code"], "plugin_job_not_available", "{answer}");
    assert_eq!(
        answer["next"],
        "Next: ask the operator to add `job` to integrations.plugins[team/plugin]"
    );
    failed(&step.row().await, "serviceUnavailable");
    step.finish().await;
}

#[tokio::test]
async fn engine_guard_refusal_completes_the_call_foreground_failed() {
    let step = step0(
        json!({"result": "done"}),
        Options {
            via: InstalledVia::SelfConfig,
            ..Default::default()
        },
    )
    .await;
    let answer = step.call(r#"{"argv": ["status"]}"#).await;
    assert_eq!(answer["code"], "plugin_job_not_available", "{answer}");
    assert!(
        answer["next"].as_str().unwrap().contains("team/plugin"),
        "{answer}"
    );
    let row = step.row().await;
    failed(&row, "serviceUnavailable");
    assert_eq!(row["await_mode"], "foreground", "{row}");
    step.finish().await;
}

#[tokio::test]
async fn job_tool_without_the_runtime_job_driver_fails_before_any_write() {
    let step = step0(
        json!({
            "result": "done",
            "writes": [{"op": "insert", "collection": "Plain", "fields": {"body": "x"}}],
        }),
        Options {
            driver: false,
            data: true,
            ..Default::default()
        },
    )
    .await;
    let answer = step.call(r#"{"argv": ["status"]}"#).await;
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("job tools are bound without the runtime's job driver"),
        "{answer}"
    );
    let row = step.row().await;
    failed(&row, "serviceUnavailable");
    assert!(row["plugin_execution_receipt"].is_null(), "{row}");
    assert!(step.bodies().await.is_empty());
    step.finish().await;
}

fn pinned_result() -> Value {
    serde_json::from_str(&format!(r#"{{"result": "done", {PIN}}}"#)).unwrap()
}

const ARGS: &str = r#"{"argv": ["run"]}"#;

/// A step 0 that adopts a job and starts one child.
fn adopting() -> Value {
    serde_json::from_str(&format!(r#"{{"effects": {{{PIN}, "spawn": [{SPAWN}]}}}}"#)).unwrap()
}

async fn finish_job(step: &Step0, row: &str) {
    let mut lifecycle = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
        step.node.clone(),
        row,
        &step.hook.node_did,
        &step.session_id,
        Some(&step.hook.node_did),
    )
    .await
    .unwrap()
    .unwrap();
    settle_plugin_job(&mut lifecycle, Settlement::Result(json!("done")))
        .await
        .unwrap();
}

#[tokio::test]
async fn adoption_commits_flip_pin_progress_effects_and_receipt_in_one_transaction() {
    let step = step0(adopting(), Options::default()).await;
    let answer = step.call(ARGS).await;
    let row = step.row().await;
    let doc = row["_docID"].as_str().unwrap();
    assert_eq!(row["lifecycle_state"], "running", "{row}");
    assert_eq!(row["await_mode"], "background", "{row}");
    assert_eq!(step.pin_of(&row).await.chain_root_doc_id, doc);
    let progress = read_progress(&step.access(), doc).await.unwrap().unwrap();
    assert_eq!(progress.revision, 1);
    assert_eq!(progress.title, "s");
    assert_eq!(progress.state, JobState::Running);
    assert_eq!(progress.spawned, 1);
    assert_eq!(
        (
            progress.budget.total,
            progress.budget.spent,
            progress.budget.remaining
        ),
        (2, 1, 1)
    );
    let child = step.child_request(doc, "s").await;
    assert_eq!(child["session_id"], job_session_id(doc, "s", 0));
    assert_eq!(child["agent_id"], "worker");
    assert_eq!(answer["status"], "running", "{answer}");
    assert_eq!(step.stored_reply(&row).await, answer);
    step.finish().await;
}

#[tokio::test]
async fn ninth_adoption_on_one_request_is_refused() {
    let step = step0(adopting(), Options::default()).await;
    for n in 1..=8 {
        let answer = step.call_as(&format!("call-{n}"), ARGS).await;
        assert_eq!(answer["status"], "running", "call {n}: {answer}");
    }
    let ninth = step.call_as("call-9", ARGS).await;
    assert_eq!(ninth["code"], "background_tool_budget_exceeded", "{ninth}");
    let row = step.row_of("call-9").await;
    failed(&row, "serviceUnavailable");
    assert_eq!(row["await_mode"], "foreground", "{row}");
    step.finish().await;
}

#[tokio::test]
async fn host_facts_overwrite_guest_pin_fields() {
    let step = step0(adopting(), Options::default()).await;
    step.call_as("call-1", ARGS).await;
    step.call_as("call-2", ARGS).await;
    step.call_as("call-3", r#"{"argv": ["run"], "args": {"k": "other"}}"#)
        .await;
    let (first, second, third) = (
        step.row_of("call-1").await,
        step.row_of("call-2").await,
        step.row_of("call-3").await,
    );
    let pin = step.pin_of(&first).await;
    let installed = store::read_record(step.home.path(), "team", "plugin").unwrap();
    assert_eq!(pin.plugin_ref, "team/plugin");
    assert_eq!(pin.engine_digest, installed.digest);
    assert_eq!(pin.origin, gents_protocol::job::JobOrigin::Dynamic);
    assert_eq!(pin.chain_root_doc_id, first["_docID"].as_str().unwrap());
    assert_eq!(
        (pin.resume_of.clone(), pin.replayed_from.clone()),
        (None, None)
    );
    assert_eq!(pin.cwd, "");
    assert!(pin.adopted_at > 1_700_000_000_000, "{}", pin.adopted_at);
    assert_eq!(
        pin.ceiling_at_pin,
        crate::plugin_job::settings::effective_ceiling(Some(&worker_settings(running_policy())))
    );
    assert_eq!(
        (
            pin.script_id.as_str(),
            pin.source_digest.as_str(),
            pin.agent_budget
        ),
        ("s", "sha256:s", 2)
    );
    assert!(
        pin.args_digest.starts_with("sha256:"),
        "{}",
        pin.args_digest
    );
    assert_eq!(pin.args_digest, step.pin_of(&second).await.args_digest);
    assert_ne!(pin.args_digest, step.pin_of(&third).await.args_digest);
    step.finish().await;
}

#[tokio::test]
async fn resume_row_copies_the_chain_root_args_digest_handle_and_chain_root() {
    let step = step0(
        adopting(),
        Options {
            idle: true,
            ..Default::default()
        },
    )
    .await;
    step.call_as("call-1", r#"{"argv": ["run"], "args": {"k": "root"}}"#)
        .await;
    let root = step.row_of("call-1").await;
    let root_id = root["_docID"].as_str().unwrap().to_owned();
    let root_pin = step.pin_of(&root).await;
    let child = step.child_request(&root_id, "s").await;
    crate::tool_call_lifecycle::admission_fixture::complete_child(
        &step.node,
        child["request_id"].as_str().unwrap(),
        &step.hook.node_did,
        "done",
    )
    .await;
    finish_job(&step, &root_id).await;
    assert_eq!(step.row_of("call-1").await["lifecycle_state"], "completed");

    let resume = json!({"row_doc_id": root_id, "engine": "current"});
    step.reinstall(&json!({"effects": {
        "pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 2, "handle": "ignored"},
        "resume": resume,
        "spawn": [{"slot": "s2", "attempt": 0, "target": "t-worker", "prompt": "go", "title": "t"}],
    }}));
    let answer = step
        .call_as("call-2", r#"{"argv": ["resume", "root"]}"#)
        .await;
    assert_eq!(answer["status"], "running", "{answer}");
    let next = step.row_of("call-2").await;
    let pin = step.pin_of(&next).await;
    assert_eq!(pin.handle, root_pin.handle);
    assert_eq!(pin.chain_root_doc_id, root_id);
    assert_eq!(pin.args_digest, root_pin.args_digest);
    assert_eq!(pin.resume_of.as_deref(), Some(root_id.as_str()));
    let state = read_step_state(&step.access(), next["_docID"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.resume, Some(serde_json::from_value(resume).unwrap()));

    let again = step
        .call_as("call-3", r#"{"argv": ["resume", "root"]}"#)
        .await;
    assert_eq!(again["code"], "plugin_job_resume_not_head", "{again}");
    let address = format!("{}:call-2", step.session_id);
    assert_eq!(
        again["next"],
        format!("Next: `{TOOL} resume {address}`"),
        "{again}"
    );
    failed(&step.row_of("call-3").await, "policyDenied");
    step.finish().await;
}

/// A step 0 that adopts a job and asks to resume `row`.
fn resuming(row: &str) -> Value {
    json!({"effects": {
        "pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 2, "handle": "h"},
        "resume": {"row_doc_id": row, "engine": "current"},
        "spawn": [{"slot": "s2", "attempt": 0, "target": "worker", "prompt": "go", "title": "t"}],
    }})
}

#[tokio::test]
async fn resume_of_a_missing_row_names_the_cause_and_the_next_call() {
    let step = step0(resuming("no-such-row"), idle()).await;
    let refused = step
        .call_as("call-1", r#"{"argv": ["resume", "gone"]}"#)
        .await;
    assert_eq!(refused["code"], "plugin_job_resume_unknown", "{refused}");
    assert_eq!(
        refused["message"], "no job row no-such-row exists to resume",
        "{refused}"
    );
    assert_eq!(
        refused["next"],
        format!("Next: `{TOOL} resume <address>` with the address the run's receipt printed"),
        "{refused}"
    );
    failed(&step.row_of("call-1").await, "policyDenied");
    step.finish().await;
}

#[tokio::test]
async fn resume_of_a_continued_row_names_the_newest_rows_address() {
    let step = step0(adopting(), idle()).await;
    step.call_as("call-1", ARGS).await;
    let root = step.row_of("call-1").await["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    // The continuation row is pinned directly; a live resume is not what is under test.
    step.reinstall(&json!({"result": "x"}));
    step.call_as("call-2", ARGS).await;
    let continuation = step.row_of("call-2").await["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    let pin = JobPin {
        plugin_ref: "team/plugin".to_owned(),
        chain_root_doc_id: root.clone(),
        resume_of: Some(root.clone()),
        ..fixture_job_pin()
    };
    crate::config_client::ConfigAccess::transact_local_write(&step.node, None, "test.pin", |txn| {
        let (doc, pin) = (&continuation, &pin);
        Box::pin(async move { write_pin_in_txn(txn, doc, pin).await })
    })
    .await
    .unwrap();

    step.reinstall(&resuming(&root));
    let refused = step
        .call_as("call-3", r#"{"argv": ["resume", "root"]}"#)
        .await;
    assert_eq!(refused["code"], "plugin_job_resume_not_head", "{refused}");
    let address = format!("{}:call-2", step.session_id);
    assert_eq!(
        refused["message"],
        format!("{root} was already resumed by {address}; only a run's newest row resumes"),
        "{refused}"
    );
    assert_eq!(
        refused["next"],
        format!("Next: `{TOOL} resume {address}`"),
        "{refused}"
    );
    failed(&step.row_of("call-3").await, "policyDenied");
    step.finish().await;
}

#[tokio::test]
async fn duplicate_handle_is_suffixed_and_the_scratch_path_is_unchanged() {
    let output = json!({"effects": {
        "pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 2, "handle": "h"},
        "spawn": [{"slot": "s", "attempt": 0, "target": "t-worker", "prompt": "go", "title": "t"}],
        "scratch": [{"name": "a.txt", "content": "x"}],
    }});
    let step = step0(
        output,
        Options {
            cwd: true,
            ..Default::default()
        },
    )
    .await;
    step.call_as("call-1", ARGS).await;
    step.call_as("call-2", ARGS).await;
    let handles = [
        step.pin_of(&step.row_of("call-1").await).await.handle,
        step.pin_of(&step.row_of("call-2").await).await.handle,
    ];
    assert_eq!(handles, ["h", "h-2"]);
    let runs = step
        .cwd
        .as_ref()
        .unwrap()
        .path()
        .join(".gents/workflows/runs");
    for call in ["call-1", "call-2"] {
        assert_eq!(
            std::fs::read_to_string(runs.join(call).join("a.txt")).unwrap(),
            "x"
        );
    }
    for handle in handles {
        assert!(!runs.join(&handle).exists(), "{handle}");
    }
    step.finish().await;
}

#[tokio::test]
async fn adoption_writes_no_execution_receipt() {
    let step = step0(adopting(), Options::default()).await;
    step.call(ARGS).await;
    let row = step.row().await;
    assert!(row["plugin_execution_receipt"].is_null(), "{row}");
    let state = read_step_state(&step.access(), row["_docID"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let evidence = state.evidence.expect("step 0's evidence is stored");
    assert_eq!(evidence.coordinate, "team/plugin");
    assert!(evidence.output_digest.is_some());
    assert_eq!(state.resume, None);
    step.finish().await;
}

#[tokio::test]
async fn background_receipt_shows_address_handle_script_id_and_tier_line() {
    let output = json!({"effects": {
        "pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 2, "handle": "h"},
        "spawn": [{"slot": "s", "attempt": 0, "target": "t-worker", "prompt": "go", "title": "t"}],
        "progress": {
            "title": "flow", "state": "running", "phases": [], "active": [], "spawned": 0,
            "budget": {"total": 2, "spent": 0, "remaining": 2},
            "last_event": "dynamic script, run mode", "observed_terminal_children": 0,
        },
    }});
    let step = step0(output, Options::default()).await;
    let answer = step.call(ARGS).await;
    assert_eq!(answer["address"], format!("{}:call-1", step.session_id));
    assert_eq!(answer["handle"], "h");
    assert_eq!(answer["script_id"], "s");
    assert_eq!(answer["tier"], "dynamic script, run mode");
    let row = step.row().await;
    let progress = read_progress(&step.access(), row["_docID"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.title, "flow");
    assert_eq!(progress.last_event, "dynamic script, run mode");
    step.finish().await;
}

#[tokio::test]
async fn adopted_origin_follows_classify() {
    const SCRIPT: &[u8] = b"fn step(input) { input }";
    let digest = format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(SCRIPT)
    );
    let installed = json!({"effects": {
        "pin": {"script_id": "s", "source_digest": digest, "agent_budget": 2, "handle": "h"},
        "spawn": [{"slot": "s", "attempt": 0, "target": "t-worker", "prompt": "go", "title": "t"}],
    }});
    let step = step0(
        installed,
        Options {
            settings: Some(worker_settings(JobPolicy {
                trusted_packs: vec!["acme/op".into()],
                ..running_policy()
            })),
            ..Default::default()
        },
    )
    .await;
    let assets = "packs/.materialized/acme/op/22";
    let dir = step.home.path().join(assets).join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("flow.rhai"), SCRIPT).unwrap();
    write_home_install(
        step.home.path(),
        &HomePackInstall {
            coordinate: "acme/op".into(),
            version: "1.0.0".into(),
            digest: "sha256:22".into(),
            kind: PackKind::Documents,
            assets: assets.into(),
            dependency_assets: vec![],
            plugins: vec![],
            ux: Vec::new(),
            installed_at: "2026-01-01T00:00:00Z".into(),
            installed_via: Some(InstalledVia::Operator),
        },
    )
    .unwrap();
    step.call_as("call-1", ARGS).await;
    assert_eq!(
        step.pin_of(&step.row_of("call-1").await).await.origin,
        gents_protocol::job::JobOrigin::Installed
    );
    step.reinstall(&adopting());
    step.call_as("call-2", ARGS).await;
    assert_eq!(
        step.pin_of(&step.row_of("call-2").await).await.origin,
        gents_protocol::job::JobOrigin::Dynamic
    );
    step.finish().await;
}

/// A step 0 answer `body` (the object's members) with the guest pin beside it.
fn pinned(body: &str) -> Value {
    serde_json::from_str(&format!("{{{body}, {PIN}}}")).unwrap()
}

impl Step0 {
    /// The Continuation completions appended for the tool call row.
    async fn continuation_notices(&self, row: &Value) -> usize {
        let key = crate::background_completion::background_completion_notification_message_key(
            row["_docID"].as_str().unwrap(),
            "tool",
        );
        let notices = self
            .access()
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(&key),
            ))
            .await
            .unwrap();
        notices["data"]["AgentMessage"].as_array().unwrap().len()
    }
}

#[tokio::test]
async fn step0_budget_pause_with_a_pin_settles_paused_inline_and_is_resumable() {
    let step = step0(
        pinned(r#""suspend": {"kind": "pause", "reason": "budget_exceeded", "message": "out of budget"}"#),
        Options::default(),
    )
    .await;
    let answer = step.call(ARGS).await;
    assert_eq!(
        answer,
        json!({"status": "paused", "reason": "budget_exceeded", "message": "out of budget"})
    );
    let row = step.row().await;
    let doc = row["_docID"].as_str().unwrap().to_owned();
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(row["await_mode"], "background", "{row}");
    let progress = read_progress(&step.access(), &doc).await.unwrap().unwrap();
    assert_eq!(progress.state, JobState::Paused);
    assert_eq!(
        progress.pause_reason,
        Some(gents_protocol::job::JobPauseReason::BudgetExceeded)
    );
    assert_eq!(step.stored_reply(&row).await, answer);
    assert_eq!(step.continuation_notices(&row).await, 0);

    step.reinstall(&json!({"effects": {
        "pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 3, "handle": "h"},
        "resume": {"row_doc_id": doc, "engine": "current", "budget": 3},
        "spawn": [{"slot": "s2", "attempt": 0, "target": "t-worker", "prompt": "go", "title": "t"}],
    }}));
    let resumed = step.call_as("call-2", r#"{"argv": ["resume", "h"]}"#).await;
    assert_eq!(resumed["status"], "running", "{resumed}");
    let next = step.pin_of(&step.row_of("call-2").await).await;
    assert_eq!((next.agent_budget, next.chain_root_doc_id), (3, doc));
    step.finish().await;
}

#[tokio::test]
async fn step0_result_with_scratch_and_a_pin_completes_with_the_file_present() {
    let step = step0(
        pinned(r#""result": "done", "scratch": [{"name": "a.txt", "content": "x"}]"#),
        Options {
            cwd: true,
            ..Default::default()
        },
    )
    .await;
    let answer = step.call(ARGS).await;
    assert_eq!(answer, json!("done"));
    let row = step.row().await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(
        std::fs::read_to_string(
            step.cwd
                .as_ref()
                .unwrap()
                .path()
                .join(".gents/workflows/runs/call-1/a.txt")
        )
        .unwrap(),
        "x"
    );
    step.finish().await;
}

#[tokio::test]
async fn two_consecutive_awaits_adopt_a_new_terminal_chain_row_inline() {
    let step = step0(
        pinned(r#""suspend": {"kind": "await_user", "message": "m", "question": "first?"}"#),
        Options::default(),
    )
    .await;
    let first = step.call(ARGS).await;
    assert_eq!(first["status"], "awaiting_user", "{first}");
    let root = step.row().await;
    let root_id = root["_docID"].as_str().unwrap().to_owned();
    let root_pin = step.pin_of(&root).await;

    let block = gents_protocol::job::JobResume {
        row_doc_id: root_id.clone(),
        engine: Some(gents_protocol::job::JobResumeEngine::Current),
        answer: Some("yes".into()),
        budget: None,
    };
    step.reinstall(&pinned(&format!(
        r#""suspend": {{"kind": "await_user", "message": "m", "question": "second?"}}, "resume": {}"#,
        serde_json::to_string(&block).unwrap()
    )));
    let second = step.call_as("call-2", r#"{"argv": ["resume", "h"]}"#).await;
    assert_eq!(second["status"], "awaiting_user", "{second}");
    assert_eq!(second["question"], "second?", "{second}");
    let row = step.row_of("call-2").await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(step.stored_reply(&row).await, second);
    assert_eq!(step.continuation_notices(&row).await, 0);
    let next_id = row["_docID"].as_str().unwrap();
    let pin = step.pin_of(&row).await;
    assert_eq!(pin.handle, root_pin.handle);
    assert_eq!(pin.chain_root_doc_id, root_id);
    assert_eq!(pin.resume_of.as_deref(), Some(root_id.as_str()));
    let state = read_step_state(&step.access(), next_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.resume, Some(block.clone()));
    let (input, _) = crate::plugin_job::input::build_step_input(
        &step.access(),
        step.hook.job_driver.as_ref().unwrap().runtime(),
        next_id,
        crate::plugin_job::input::StepKind::Later,
    )
    .await
    .unwrap();
    assert_eq!(
        input.control,
        Some(crate::plugin_job::control::resume_echo(&block))
    );
    step.finish().await;
}

#[tokio::test]
async fn inline_settlement_writes_exactly_one_receipt() {
    let step = step0(pinned(r#""result": "done""#), Options::default()).await;
    let answer = step.call(ARGS).await;
    assert_eq!(answer, json!("done"));
    let row = step.row().await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    let state = read_step_state(&step.access(), row["_docID"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    let stored: gents_protocol::plugin::PluginExecutionReceipt =
        serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap();
    assert_eq!(Some(stored), state.evidence);
    step.finish().await;
}

#[tokio::test]
async fn pinned_step0_terminal_adopts_and_settles_in_one_transaction() {
    let step = step0(pinned(r#""result": "done""#), Options::default()).await;
    accept_hook_tool_call(&step.hook, "call-1", TOOL, ARGS, None).await;
    let row = step.row().await;
    let doc = row["_docID"].as_str().unwrap().to_owned();
    crate::tool_call_lifecycle::delivery::fail_next_terminal_for_test(&doc);
    let action = step.hook.on_tool_call(TOOL, None, "call-1", ARGS).await;
    let ToolCallHookAction::Skip { reason } = action else {
        panic!("a job tool call is answered by its hook: {action:?}");
    };
    assert_eq!(reason, "done");
    assert!(!crate::tool_call_lifecycle::delivery::terminal_failure_pending_for_test(&doc));
    let row = step.row().await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(row["await_mode"], "background", "{row}");
    assert_eq!(step.pin_of(&row).await.chain_root_doc_id, doc);
    let progress = read_progress(&step.access(), &doc).await.unwrap().unwrap();
    assert_eq!(progress.revision, 1);
    let state = read_step_state(&step.access(), &doc)
        .await
        .unwrap()
        .unwrap();
    let stored: gents_protocol::plugin::PluginExecutionReceipt =
        serde_json::from_value(row["plugin_execution_receipt"].clone()).unwrap();
    assert_eq!(Some(stored), state.evidence);
    assert_eq!(step.continuation_notices(&row).await, 0);
    step.finish().await;
}

#[tokio::test]
async fn pinned_step0_with_a_write_outside_the_grant_completes_foreground_failed() {
    const WRITE: &str = r#"{"op": "insert", "collection": "Other", "fields": {"body": "x"}}"#;
    let outputs = [
        pinned(&format!(r#""result": "done", "writes": [{WRITE}]"#)),
        serde_json::from_str(&format!(
            r#"{{"effects": {{{PIN}, "spawn": [{SPAWN}], "writes": [{WRITE}]}}}}"#
        ))
        .unwrap(),
    ];
    for output in outputs {
        let step = step0(
            output,
            Options {
                data: true,
                ..Default::default()
            },
        )
        .await;
        let answer = step.call(ARGS).await;
        assert_eq!(answer["status"], "failed", "{answer}");
        assert!(
            answer["message"]
                .as_str()
                .unwrap()
                .contains("writes were refused and nothing was committed"),
            "{answer}"
        );
        failed(&step.row().await, "policyDenied");
        assert!(step.bodies().await.is_empty());
        step.finish().await;
    }
}

fn idle() -> Options {
    Options {
        idle: true,
        ..Default::default()
    }
}

/// A non-job answer whose `control` names each `(row, kind)`.
fn controlling(entries: &[(&str, &str)]) -> Value {
    let control: Vec<Value> = entries
        .iter()
        .map(|(row, kind)| json!({"row_doc_id": row, "kind": kind}))
        .collect();
    json!({"result": "controlled", "control": control})
}

impl Step0 {
    async fn interrupted(&self, doc_id: &str) -> bool {
        crate::interrupt::fetch_interrupt_requested_at_by_doc_id(&self.node, doc_id)
            .await
            .unwrap()
            .is_some()
    }

    /// Adopts a running job with one live child; returns the job row and
    /// the child request's document ids.
    async fn running_job(&self) -> (String, String) {
        let answer = self.call(ARGS).await;
        assert_eq!(answer["status"], "running", "{answer}");
        let job = self.row().await["_docID"].as_str().unwrap().to_owned();
        // The step the adoption kicked ends with a second progress revision;
        // a reinstall before then would make it fail the job closed.
        while read_progress(&self.access(), &job)
            .await
            .unwrap()
            .unwrap()
            .revision
            < 2
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let child = self.child_request(&job, "s").await["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        (job, child)
    }

    /// Ends the non-job call `call_id` and gives its row a job pin of
    /// another plugin in the calling session.
    async fn other_plugin_job(&self, call_id: &str) -> String {
        self.reinstall(&json!({"result": "x"}));
        self.call_as(call_id, ARGS).await;
        let doc = self.row_of(call_id).await["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        write_fixture_job_pin(&self.node, &doc).await.unwrap();
        doc
    }

    async fn state_of(&self, call_id: &str) -> String {
        self.row_of(call_id).await["lifecycle_state"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[tokio::test]
async fn non_job_stop_ends_an_own_running_job_cancelled_and_interrupts_its_children() {
    let step = step0(adopting(), idle()).await;
    let (job, child) = step.running_job().await;
    assert!(!step.interrupted(&child).await);
    step.reinstall(&controlling(&[(&job, "stop")]));
    step.call_as("call-2", ARGS).await;
    assert_eq!(step.state_of("call-1").await, "cancelled");
    assert!(step.interrupted(&child).await);
    assert_eq!(step.state_of("call-2").await, "completed");
    step.finish().await;
}

#[tokio::test]
async fn non_job_pause_ends_an_own_running_job_completed_paused() {
    let step = step0(adopting(), idle()).await;
    let (job, child) = step.running_job().await;
    step.reinstall(&controlling(&[(&job, "pause")]));
    step.call_as("call-2", ARGS).await;
    let row = step.row_of("call-1").await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    let progress = read_progress(&step.access(), &job).await.unwrap().unwrap();
    assert_eq!(progress.state, JobState::Paused);
    assert!(step.interrupted(&child).await);
    step.finish().await;
}

#[tokio::test]
async fn non_job_control_on_another_plugin_or_out_of_scope_row_is_refused() {
    let step = step0(json!({"result": "x"}), Options::default()).await;
    let other_plugin = step.other_plugin_job("call-1").await;
    let (root_request, other_session) = job_ancestry(&step.node, 1).await;
    // A job of this very plugin in a session the caller does not own: only
    // the scope check refuses it.
    let out_of_scope = insert(
        &step.node,
        "AgentToolCall",
        json!({
            "request_id": "root",
            "request_doc_id": root_request,
            "node_did": "owner",
            "requester_did": null,
            "session_id": "root",
            "tool_call_id": "foreign",
            "tool_name": "agent_new",
            "lifecycle_state": "running",
            "await_mode": "background",
        }),
    )
    .await;
    let pin = JobPin {
        plugin_ref: "team/plugin".to_owned(),
        ..fixture_job_pin()
    };
    crate::config_client::ConfigAccess::transact_local_write(&step.node, None, "test.pin", |txn| {
        let (doc, pin) = (&out_of_scope, &pin);
        Box::pin(async move { write_pin_in_txn(txn, doc, pin).await })
    })
    .await
    .unwrap();
    for (n, target) in [
        &other_plugin,
        &other_session,
        &out_of_scope,
        &"no-such-row".to_owned(),
        &step.row().await["_docID"].as_str().unwrap().to_owned(),
    ]
    .into_iter()
    .enumerate()
    {
        step.reinstall(&controlling(&[(target, "stop")]));
        let call = format!("call-{}", n + 2);
        let answer = step.call_as(&call, ARGS).await;
        assert_eq!(
            answer["code"], "plugin_job_policy_denied",
            "{target}: {answer}"
        );
        failed(&step.row_of(&call).await, "policyDenied");
    }
    assert_eq!(step.state_of("call-1").await, "completed");
    step.finish().await;
}

#[tokio::test]
async fn calling_row_of_a_control_answer_is_never_adopted() {
    let step = step0(adopting(), idle()).await;
    let (job, _) = step.running_job().await;
    step.reinstall(&controlling(&[(&job, "stop")]));
    step.call_as("call-2", ARGS).await;
    let row = step.row_of("call-2").await;
    assert_eq!(row["lifecycle_state"], "completed", "{row}");
    assert_eq!(row["await_mode"], "foreground", "{row}");
    assert!(row["job_pin"].is_null(), "{row}");
    step.finish().await;
}

#[tokio::test]
async fn an_unauthorized_later_entry_kills_nothing() {
    let step = step0(adopting(), idle()).await;
    let (job, child) = step.running_job().await;
    let other = step.other_plugin_job("call-2").await;
    step.reinstall(&controlling(&[(&job, "stop"), (&other, "stop")]));
    let answer = step.call_as("call-3", ARGS).await;
    assert_eq!(answer["code"], "plugin_job_policy_denied", "{answer}");
    failed(&step.row_of("call-3").await, "policyDenied");
    assert_eq!(step.state_of("call-1").await, "running");
    assert!(!step.interrupted(&child).await);
    step.finish().await;
}
