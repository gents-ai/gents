use super::super::*;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle;

pub(super) use crate::documents::Tools;
pub(super) use crate::ensure_runtime_schemas;
pub(super) use crate::request::watcher::AgentRequest;
pub(super) use crate::runtime::status::RuntimeStatusHandle;
pub(super) use crate::store::graphql::escape_graphql_string;
pub(super) use crate::tools::surface::ToolCeiling;
pub(super) use gents_protocol::node_readiness::{
    AgentReadinessState, AgentReadinessUnavailableReason, NodeReadinessProcessState,
    NodeReadinessSnapshot,
};
pub(super) use serde_json::Value;

pub(super) fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

pub(super) struct PluginRuntimeFixture {
    pub node: Arc<defra_node::Node>,
    pub runtime: Arc<KeyIdentity>,
    pub user_access: crate::store::config_client::ConfigAccess,
    pub agent_access: crate::store::config_client::ConfigAccess,
    pub node_binding: crate::store::config_client::ConfigurationNodeBinding,
}

pub(super) async fn plugin_runtime_fixture(
    name: &str,
    backend_id: &str,
    endpoint: &str,
) -> anyhow::Result<PluginRuntimeFixture> {
    use crate::store::config_client::{
        ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan, GraphqlEndpoint,
        NodePermission,
    };
    use crate::Collection;
    use serde_json::json;

    let runtime = Arc::new(test_identity(&format!("{name}-runtime")));
    let user = test_identity(&format!("{name}-user"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    crate::ensure_runtime_schemas(&node).await?;
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    let user_did = identity::Did::new(user.did())?;
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        admin.grant_node_permission(&user_did, permission).await?;
    }
    let user_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    let node_value = json!({"node_did":runtime.did(), "enabled":true});
    let node_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Node,
        add: node_value.clone(),
        update: node_value,
    }])?;
    user_access
        .transact_write("test.plugin.node", |txn| {
            let plan = &node_plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    let binding = crate::store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await?;
    let agent = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let agent_did = binding.agent_did.clone();
    let context_id = format!("{agent_did}:context");
    let tools_id = format!("{agent_did}:tools");
    let profile_id = format!("{agent_did}:inference");
    let docs = [
        (Collection::Agent, json!({"node_did":runtime.did(), "agent_did":agent_did, "creation_nonce":binding.creation_nonce, "context_id":context_id, "inference_profile_id":profile_id})),
        (Collection::AgentContext, json!({"node_did":runtime.did(), "context_id":context_id, "tools_id":tools_id})),
        (Collection::Tools, json!({"node_did":runtime.did(), "tools_id":tools_id})),
        (Collection::InferenceProfile, json!({"node_did":runtime.did(), "profile_id":profile_id, "backend_id":backend_id, "model_name":"default"})),
        (Collection::InferenceBackend, json!({"node_did":runtime.did(), "backend_id":backend_id, "name":backend_id, "provider_kind":"OpenAiCompatible", "endpoint":endpoint, "max_concurrent":1, "auth":{"kind":"unauthenticated"}})),
    ].into_iter().map(|(collection, value)| DesiredStateApplyDocument { collection, add: value.clone(), update: value }).collect();
    let plan = DesiredStateApplyPlan::new(docs)?;
    user_access
        .transact_write("test.plugin.configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    let observed = user_access.execute("{ Node { _docID node_did } }").await?;
    let node_doc = observed["data"]["Node"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["node_did"] == runtime.did()))
        .and_then(|row| row["_docID"].as_str())
        .ok_or_else(|| anyhow::anyhow!("plugin fixture Node missing"))?
        .to_owned();
    let default_node =
        json!({"node_did":runtime.did(), "enabled":true, "default_agent_did":agent_did});
    let default_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Node,
        add: default_node.clone(),
        update: default_node,
    }])?;
    user_access
        .transact_write("test.plugin.default", |txn| {
            let plan = &default_plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let node_binding = crate::store::config_client::ConfigurationNodeBinding {
        node_did: identity::Did::new(runtime.did())?,
        doc_id: node_doc,
        enabled: true,
        default_agent_did: Some(agent_did),
    };
    Ok(PluginRuntimeFixture {
        node,
        runtime,
        user_access,
        agent_access,
        node_binding,
    })
}

pub(super) struct HeldRuntimeFixture {
    pub db: crate::support::TestDb,
    pub user: KeyIdentity,
    pub user_access: crate::ConfigAccess,
    pub agent: std::sync::Arc<dyn NodeIdentity>,
}

impl HeldRuntimeFixture {
    pub fn admission(&self) -> crate::store::local::backend_protection::BackendAdmission {
        self.db.home.backend_admission()
    }

    pub async fn agent_config_access(&self) -> crate::ConfigAccess {
        use crate::store::config_client::NodePermission;
        let runtime_did = self.db.node_identity.did();
        let admin =
            crate::ConfigAccess::local_as(self.db.node.clone(), runtime_did.parse().unwrap());
        let agent_did = identity::Did::new(self.agent.did()).unwrap();
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
        ] {
            admin
                .grant_node_permission(&agent_did, permission)
                .await
                .unwrap();
        }
        self.user_access
            .transact_write("test.runtime.agent_config", |txn| {
                let agent_did = &agent_did;
                Box::pin(async move {
                    crate::store::config_client::grant_selected_self_configuration(
                        txn,
                        runtime_did,
                        agent_did.as_str(),
                    )
                    .await
                })
            })
            .await
            .unwrap();
        crate::ConfigAccess::local_managed_agent(
            self.db.node.clone(),
            agent_did,
            self.agent.as_ref(),
        )
        .unwrap()
    }
}

pub(super) async fn held_runtime_fixture(
    name: &str,
    backend_id: &str,
    endpoint: &str,
) -> HeldRuntimeFixture {
    held_runtime_fixture_with_status(name, backend_id, endpoint, 1, "healthy").await
}

pub(super) async fn held_runtime_fixture_with_status(
    name: &str,
    backend_id: &str,
    endpoint: &str,
    max_concurrent: i64,
    probe_status: &str,
) -> HeldRuntimeFixture {
    use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let (db, user, user_access, binding) = crate::test_support::signed_test_node(name).await;
    let node_did = db.node_identity.did();
    let profile_id = format!("{}:inference", binding.agent_did);
    let docs = [
        (
            crate::Collection::InferenceProfile,
            serde_json::json!({
                "node_did": node_did, "profile_id": profile_id,
                "backend_id": backend_id, "model_name": "default"
            }),
        ),
        (
            crate::Collection::InferenceBackend,
            serde_json::json!({
                "node_did": node_did, "backend_id": backend_id,
                "name": backend_id, "provider_kind": "OpenAiCompatible", "endpoint": endpoint,
                "max_concurrent": max_concurrent, "auth": {"kind": "unauthenticated"}
            }),
        ),
    ]
    .into_iter()
    .map(|(collection, value)| DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    })
    .collect();
    let plan = DesiredStateApplyPlan::new(docs).unwrap();
    user_access
        .transact_write("test.runtime.inference", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
    let runtime_access = crate::ConfigAccess::local_as(db.node.clone(), node_did.parse().unwrap());
    crate::inference::backends::registry::set_backend_probe_status_as(
        &runtime_access,
        node_did,
        backend_id,
        probe_status,
    )
    .await
    .unwrap();
    let agent = crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
        .unwrap();
    grant_held_agent_operations(&db.node, db.node_identity.as_ref(), agent.as_ref()).await;
    HeldRuntimeFixture {
        db,
        user,
        user_access,
        agent,
    }
}

pub(super) async fn grant_held_agent_operations(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    agent: &dyn NodeIdentity,
) {
    use crate::store::config_client::NodePermission;
    let admin = crate::ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
    let agent_did = identity::Did::new(agent.did()).unwrap();
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
}

pub(super) async fn create_held_agent_request(
    node: &Arc<defra_node::Node>,
    node_did: &str,
    agent: &dyn NodeIdentity,
    request_id: &str,
    session_id: &str,
    content: &str,
) -> String {
    let did = agent.did();
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        node_did,
        did,
        did,
        session_id,
        content,
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(did),
    );
    crate::sign_agent_request_create(agent, &mut create)
        .await
        .unwrap();
    let access =
        crate::ConfigAccess::local_managed_agent(node.clone(), did.parse().unwrap(), agent)
            .unwrap();
    let response = access
        .write(
            "test.runtime.create_request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

pub(super) fn request(agent_did: Option<&str>, session_id: &str) -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: "doc-1".to_string(),
        request_id: "req-1".to_string(),
        node_did: "did:test:test".to_string(),
        requester_did: None,
        agent_did: agent_did.unwrap_or_default().to_owned(),
        session_id: session_id.to_string(),
        content: "hello".to_string(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: None,
        created_at: "2026-04-09T00:00:00Z".to_string(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_node_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}

#[derive(Debug)]
pub(super) struct RuntimeStatusRow {
    pub(super) process_state: String,
    pub(super) active_generation: i64,
    pub(super) reconcile_phase: String,
    pub(super) last_reconcile_result: String,
    pub(super) last_reconcile_error: String,
    pub(super) updated_at: String,
}

#[derive(serde::Deserialize)]
struct RuntimeDiagnosticRow {
    reconcile_phase: String,
    last_reconcile_result: String,
    last_reconcile_error: String,
    updated_at: String,
}

pub(super) async fn fetch_runtime_status(
    node: &defra_node::Node,
    node_did: &str,
) -> RuntimeStatusRow {
    fetch_runtime_status_if_present(node, node_did)
        .await
        .expect("NodeRuntime row")
}

async fn fetch_runtime_status_if_present(
    node: &defra_node::Node,
    node_did: &str,
) -> Option<RuntimeStatusRow> {
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeRuntime(filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }}, limit: 1) {{
                reconcile_phase
                last_reconcile_result
                last_reconcile_error
                updated_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "NodeRuntime query failed: {:?}",
        response.errors
    );
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("NodeRuntime"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()?;
    let diagnostic: RuntimeDiagnosticRow =
        serde_json::from_value(value).expect("decode NodeRuntime row");
    let readiness = fetch_node_readiness(node, node_did).await;
    Some(RuntimeStatusRow {
        process_state: readiness.process_state.as_str().to_string(),
        active_generation: i64::try_from(readiness.active_generation).unwrap_or(i64::MAX),
        reconcile_phase: diagnostic.reconcile_phase,
        last_reconcile_result: diagnostic.last_reconcile_result,
        last_reconcile_error: diagnostic.last_reconcile_error,
        updated_at: diagnostic.updated_at,
    })
}

pub(super) async fn wait_for_runtime_reconcile_phase(
    node: &defra_node::Node,
    node_did: &str,
    expected_reconcile_phase: &str,
) -> RuntimeStatusRow {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(row) = fetch_runtime_status_if_present(node, node_did).await {
            if row.reconcile_phase == expected_reconcile_phase {
                return row;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for NodeRuntime {} to reach reconcile_phase={}",
            node_did,
            expected_reconcile_phase
        );
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }
}

pub(super) async fn fetch_node_readiness(
    node: &defra_node::Node,
    node_did: &str,
) -> NodeReadinessSnapshot {
    let escaped_node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeReadiness(
                filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                limit: 1
            ) {{
                snapshot_json
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "NodeReadiness query failed: {:?}",
        response.errors
    );
    let snapshot_json = response
        .data
        .as_ref()
        .and_then(|data| data.get("NodeReadiness"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("snapshot_json"))
        .and_then(Value::as_str)
        .expect("NodeReadiness row");
    serde_json::from_str(snapshot_json).expect("decode node readiness snapshot")
}

pub(super) async fn wait_for_runtime_process_state(
    node: &defra_node::Node,
    node_did: &str,
    expected_process_state: &str,
) {
    let escaped_node_did = escape_graphql_string(node_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let query = format!(
            r#"{{
                NodeReadiness(filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }}, limit: 1) {{
                    snapshot_json
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "NodeReadiness query failed: {:?}",
            response.errors
        );
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("NodeReadiness"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("snapshot_json"))
            .and_then(Value::as_str)
            .and_then(|snapshot| serde_json::from_str::<NodeReadinessSnapshot>(snapshot).ok())
            .map(|snapshot| snapshot.process_state);
        if process_state.map(NodeReadinessProcessState::as_str) == Some(expected_process_state) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for NodeReadiness {} to reach process_state={}; last={:?}",
            node_did,
            expected_process_state,
            process_state
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub(super) struct MockModelEndpoint {
    pub(super) endpoint: String,
    pub(super) port: u16,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) handle: Option<JoinHandle<()>>,
}

impl MockModelEndpoint {
    pub(super) fn start(model_name: &str) -> anyhow::Result<Self> {
        Self::start_with_blocking_chat(model_name, false)
    }

    pub(super) fn start_blocking_chat(model_name: &str) -> anyhow::Result<Self> {
        Self::start_with_blocking_chat(model_name, true)
    }

    fn start_with_blocking_chat(model_name: &str, blocking_chat: bool) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        // Drop wakes the blocking accept with a self-connection.
        let handle = thread::spawn(move || {
            let mut held_chats = Vec::new();
            for stream in listener.incoming() {
                if stop_for_thread.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut stream) = stream else { break };
                let request = match read_http_request(&mut stream) {
                    Ok(request) => request,
                    Err(_) => {
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }
                };
                if blocking_chat
                    && request.method == "POST"
                    && request.path == "/v1/chat/completions"
                {
                    held_chats.push(stream);
                    continue;
                }

                let (status, body) = if request.method == "GET"
                    && (request.path == "/v1/models" || request.path == "/models")
                {
                    ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
                } else {
                    ("404 Not Found", r#"{"error":"not found"}"#.to_string())
                };
                let _ = write_http_response(&mut stream, status, "application/json", &body);
                let _ = stream.shutdown(Shutdown::Both);
            }
            for stream in held_chats {
                let _ = stream.shutdown(Shutdown::Both);
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            handle: Some(handle),
        })
    }

    pub(super) fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(super) struct HttpRequestData {
    pub(super) method: String,
    pub(super) path: String,
}

pub(super) fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<HttpRequestData> {
    // macOS accepts inherit the listener's nonblocking mode; the read timeout
    // alone cannot stop an early WouldBlock from closing the fixture socket.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buffer = Vec::new();
    let mut temp = [0u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            anyhow::bail!("connection closed before headers");
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request path"))?
        .to_string();
    Ok(HttpRequestData { method, path })
}

pub(super) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(super) fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> anyhow::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

#[derive(Default)]
pub(super) struct RecordingObserver {
    pub(super) states: std::sync::Mutex<Vec<crate::runtime::agent::ProcessLifecycleState>>,
}

impl crate::runtime::agent::ProcessLifecycleObserver for RecordingObserver {
    fn on_process_state_change(&self, state: crate::runtime::agent::ProcessLifecycleState) {
        self.states
            .lock()
            .expect("recording observer mutex poisoned")
            .push(state);
    }
}

pub(super) async fn update_backend_probe_status(
    node: &Arc<defra_node::Node>,
    node_did: &str,
    backend_id: &str,
    probe_status: &str,
) {
    let access = crate::ConfigAccess::local_as(node.clone(), node_did.parse().unwrap());
    crate::inference::backends::registry::set_backend_probe_status_as(
        &access,
        node_did,
        backend_id,
        probe_status,
    )
    .await
    .unwrap();
}

pub(super) struct ScriptedWatcher {
    pub(super) rx: mpsc::Receiver<anyhow::Result<AgentRequest>>,
}

impl Watcher for ScriptedWatcher {
    async fn next_request(&mut self) -> Option<anyhow::Result<AgentRequest>> {
        self.rx.recv().await
    }
}
