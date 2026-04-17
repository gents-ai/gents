pub(crate) mod stream_guard;
mod config;
mod client;
mod controller;
mod permit;
mod registry;
mod persistence;

pub(crate) use config::{backend_admission_configs_from_backends, BackendAdmissionConfig};
pub(crate) use client::{
    scope_call, scope_request, AdmissionCallContext, AdmittedCompletionClient, CallKind,
};
pub(crate) use permit::AdmissionPermit;

use std::collections::{HashMap, HashSet};
use std::future::Future;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};

use self::client::current_context;
use self::controller::{BackendAdmissionController, InferenceCallRecord, PendingCallMetadata};
use crate::graphql::escape_graphql_string;

#[derive(Clone)]
pub(crate) struct AdmissionRegistry {
    inner: Arc<AdmissionRegistryInner>,
}

struct AdmissionRegistryInner {
    node: Arc<EmbeddedNode>,
    runtime_instance_id: String,
    state: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    active: HashMap<String, Arc<BackendAdmissionController>>,
    draining: HashMap<String, Vec<Arc<BackendAdmissionController>>>,
    pending: HashMap<String, PendingControllerConfig>,
}

#[derive(Clone)]
struct PendingControllerConfig {
    generation: u64,
    config: BackendAdmissionConfig,
}

impl AdmissionRegistry {
    pub(crate) fn new(node: Arc<EmbeddedNode>) -> Self {
        Self {
            inner: Arc::new(AdmissionRegistryInner {
                node,
                runtime_instance_id: uuid::Uuid::new_v4().to_string(),
                state: Mutex::new(RegistryState::default()),
            }),
        }
    }

    pub(crate) fn reconcile(
        &self,
        generation: u64,
        configs: &HashMap<String, BackendAdmissionConfig>,
    ) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("AdmissionRegistry state lock poisoned");
        state.prune_drained();

        let desired_ids = configs.keys().cloned().collect::<HashSet<_>>();
        let active_ids = state.active.keys().cloned().collect::<Vec<_>>();
        for backend_id in active_ids {
            let desired = configs
                .get(&backend_id)
                .filter(|config| config.is_available());
            match (state.active.remove(&backend_id), desired) {
                (Some(active), Some(config)) if active.matches(config) => {
                    state.active.insert(backend_id, active);
                }
                (Some(active), Some(config)) => {
                    active.close();
                    if active.is_drained() {
                        state.active.insert(
                            backend_id.clone(),
                            BackendAdmissionController::new(
                                generation,
                                config.clone(),
                                Arc::downgrade(&self.inner),
                            ),
                        );
                    } else {
                        state
                            .draining
                            .entry(backend_id.clone())
                            .or_default()
                            .push(active);
                        state.pending.insert(
                            backend_id,
                            PendingControllerConfig {
                                generation,
                                config: config.clone(),
                            },
                        );
                    }
                }
                (Some(active), None) => {
                    active.close();
                    if !active.is_drained() {
                        state
                            .draining
                            .entry(backend_id.clone())
                            .or_default()
                            .push(active);
                    }
                    state.pending.remove(&backend_id);
                }
                (None, _) => {}
            }
        }

        for (backend_id, config) in configs {
            if !config.is_available() || !desired_ids.contains(backend_id) {
                state.pending.remove(backend_id);
                continue;
            }
            if state.active.contains_key(backend_id) {
                continue;
            }
            if state.has_draining(backend_id) {
                state.pending.insert(
                    backend_id.clone(),
                    PendingControllerConfig {
                        generation,
                        config: config.clone(),
                    },
                );
                continue;
            }
            state.active.insert(
                backend_id.clone(),
                BackendAdmissionController::new(generation, config.clone(), Arc::downgrade(&self.inner)),
            );
        }

        let pending_ids = state.pending.keys().cloned().collect::<Vec<_>>();
        for backend_id in pending_ids {
            state.install_pending_if_ready(&self.inner, &backend_id);
        }
    }

    #[cfg(test)]
    pub(crate) async fn acquire_for_test(
        &self,
        request_id: impl Into<String>,
        backend_id: impl Into<String>,
        behavior_id: impl Into<String>,
        agent_did: impl Into<String>,
        call_kind: CallKind,
    ) -> Result<AdmissionPermit, CompletionError> {
        let context = AdmissionCallContext {
            request_id: request_id.into(),
            backend_id: backend_id.into(),
            behavior_id: behavior_id.into(),
            agent_did: agent_did.into(),
            call_kind,
            attempt: 1,
            call_seq: Arc::new(AtomicU64::new(0)),
        };
        scope_request(context, async { self.acquire_current_call().await }).await
    }

    async fn acquire_current_call(&self) -> Result<AdmissionPermit, CompletionError> {
        let context = current_context()?;
        let pending = context.next_call(&self.inner.runtime_instance_id);
        if pending.backend_id.trim().is_empty() {
            return Err(CompletionError::ProviderError(format!(
                "behavior {} has no backend binding",
                pending.behavior_id
            )));
        }

        let controller = {
            let state = self
                .inner
                .state
                .lock()
                .expect("AdmissionRegistry state lock poisoned");
            state.active.get(&pending.backend_id).cloned()
        };

        match controller {
            Some(controller) => controller.acquire(self.inner.node.clone(), pending).await,
            None => {
                let call = InferenceCallRecord::without_controller(pending);
                if let Err(error) = persist_terminal_call(
                    self.inner.node.clone(),
                    call,
                    "cancelled",
                    Some("BackendGone"),
                    None,
                )
                .await
                {
                    tracing::warn!(error = %error, "failed to persist backend-gone inference call");
                }
                Err(CompletionError::ProviderError(
                    "BackendGone: backend admission controller is not active".into(),
                ))
            }
        }
    }
}

impl AdmissionRegistryInner {
    fn controller_drained(self: Arc<Self>, backend_id: String) {
        let mut state = self
            .state
            .lock()
            .expect("AdmissionRegistry state lock poisoned");
        state.install_pending_if_ready(&self, &backend_id);
    }
}

impl RegistryState {
    fn prune_drained(&mut self) {
        self.draining.retain(|_, controllers| {
            controllers.retain(|controller| !controller.is_drained());
            !controllers.is_empty()
        });
    }

    fn has_draining(&mut self, backend_id: &str) -> bool {
        self.prune_drained();
        self.draining
            .get(backend_id)
            .is_some_and(|controllers| !controllers.is_empty())
    }

    fn install_pending_if_ready(
        &mut self,
        registry: &Arc<AdmissionRegistryInner>,
        backend_id: &str,
    ) {
        self.prune_drained();
        if self.active.contains_key(backend_id) || self.has_draining(backend_id) {
            return;
        }
        let Some(pending) = self.pending.remove(backend_id) else {
            return;
        };
        if pending.config.is_available() {
            self.active.insert(
                backend_id.to_string(),
                BackendAdmissionController::new(pending.generation, pending.config, Arc::downgrade(registry)),
            );
        }
    }
}

fn spawn_persistence<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    }
}

fn completion_persistence_error(error: anyhow::Error) -> CompletionError {
    CompletionError::ProviderError(format!("persisting InferenceCall failed: {error:#}"))
}

fn extract_inference_call_doc_id(data: Option<&serde_json::Value>) -> Result<String> {
    data.and_then(|data| data.get("add_InferenceCall"))
        .and_then(|value| {
            value
                .get("_docID")
                .and_then(|doc_id| doc_id.as_str())
                .or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                        .and_then(|doc_id| doc_id.as_str())
                })
        })
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("add_InferenceCall returned no _docID"))
}

async fn persist_call_queued(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "queued", None, Some(&now), None, None, None);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("persisting queued InferenceCall failed: {:?}", resp.errors);
    }
    extract_inference_call_doc_id(resp.data.as_ref())
}

async fn persist_call_started(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String, CompletionError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "running", None, Some(&now), Some(&now), None, None);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        return Err(CompletionError::ProviderError(format!(
            "persisting running InferenceCall failed: {:?}",
            resp.errors
        )));
    }
    extract_inference_call_doc_id(resp.data.as_ref()).map_err(completion_persistence_error)
}

async fn persist_existing_call_running(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = upsert_call_running_mutation(call, &now);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("persisting running InferenceCall failed: {:?}", resp.errors);
    }
    Ok(())
}

async fn persist_terminal_call(
    node: Arc<EmbeddedNode>,
    call: InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<Usage>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(
        &call,
        call_state,
        failure_reason,
        Some(&now),
        None,
        Some(&now),
        usage,
    );
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "persisting terminal InferenceCall failed: {:?}",
            resp.errors
        );
    }
    Ok(())
}

async fn persist_existing_call_terminal(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<Usage>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = upsert_call_terminal_mutation(call, call_state, failure_reason, &now, usage);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "persisting terminal InferenceCall failed: {:?}",
            resp.errors
        );
    }
    Ok(())
}

fn add_call_mutation(
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    queued_at: Option<&str>,
    started_at: Option<&str>,
    ended_at: Option<&str>,
    usage: Option<Usage>,
) -> String {
    let queued_at = optional_graphql_string("queued_at", queued_at);
    let started_at = optional_graphql_string("started_at", started_at);
    let ended_at = optional_graphql_string("ended_at", ended_at);
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens) = usage_fields(usage);
    format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{call_id}",
                runtime_instance_id: "{runtime_instance_id}",
                request_id: "{request_id}",
                call_seq: {call_seq},
                backend_id: "{backend_id}",
                behavior_id: "{behavior_id}",
                agent_did: "{agent_did}",
                call_kind: "{call_kind}",
                attempt: {attempt},
                call_state: "{call_state}",
                {failure_reason}
                {queued_at}
                {started_at}
                {ended_at}
                priority: 0,
                queue_depth_at_enqueue: {queue_depth_at_enqueue},
                controller_generation: {controller_generation},
                backend_config_fingerprint: "{backend_config_fingerprint}"
                {prompt_tokens}
                {completion_tokens}
            }}) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        call_state = call_state,
        failure_reason = failure_reason,
        queued_at = queued_at,
        started_at = started_at,
        ended_at = ended_at,
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
    )
}

fn upsert_call_running_mutation(call: &InferenceCallRecord, started_at: &str) -> String {
    format!(
        r#"mutation {{
            upsert_InferenceCall(
                filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                add: {{
                    call_id: "{call_id}",
                    runtime_instance_id: "{runtime_instance_id}",
                    request_id: "{request_id}",
                    call_seq: {call_seq},
                    backend_id: "{backend_id}",
                    behavior_id: "{behavior_id}",
                    agent_did: "{agent_did}",
                    call_kind: "{call_kind}",
                    attempt: {attempt},
                    call_state: "running",
                    queued_at: "{started_at}",
                    started_at: "{started_at}",
                    priority: 0,
                    queue_depth_at_enqueue: {queue_depth_at_enqueue},
                    controller_generation: {controller_generation},
                    backend_config_fingerprint: "{backend_config_fingerprint}"
                }},
                update: {{
                    call_state: "running",
                    started_at: "{started_at}"
                }}
            ) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        started_at = escape_graphql_string(started_at),
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
    )
}

fn upsert_call_terminal_mutation(
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    ended_at: &str,
    usage: Option<Usage>,
) -> String {
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens) = usage_fields(usage);
    format!(
        r#"mutation {{
            upsert_InferenceCall(
                filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                add: {{
                    call_id: "{call_id}",
                    runtime_instance_id: "{runtime_instance_id}",
                    request_id: "{request_id}",
                    call_seq: {call_seq},
                    backend_id: "{backend_id}",
                    behavior_id: "{behavior_id}",
                    agent_did: "{agent_did}",
                    call_kind: "{call_kind}",
                    attempt: {attempt},
                    call_state: "{call_state}",
                    {failure_reason}
                    queued_at: "{ended_at}",
                    ended_at: "{ended_at}",
                    priority: 0,
                    queue_depth_at_enqueue: {queue_depth_at_enqueue},
                    controller_generation: {controller_generation},
                    backend_config_fingerprint: "{backend_config_fingerprint}"
                    {prompt_tokens}
                    {completion_tokens}
                }},
                update: {{
                    call_state: "{call_state}",
                    {failure_reason}
                    ended_at: "{ended_at}"
                    {prompt_tokens}
                    {completion_tokens}
                }}
            ) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        call_state = call_state,
        failure_reason = failure_reason,
        ended_at = escape_graphql_string(ended_at),
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
    )
}

fn optional_graphql_string(field: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{field}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

fn usage_fields(usage: Option<Usage>) -> (String, String) {
    match usage {
        Some(usage) => (
            format!("prompt_tokens: {},", usage.input_tokens),
            format!("completion_tokens: {},", usage.output_tokens),
        ),
        None => (String::new(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Value;

    use super::*;
    use crate::schema::ensure_schemas;
    use crate::watcher::AgentRequest;

    async fn test_node() -> Arc<EmbeddedNode> {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        ensure_schemas(node.as_ref()).await.unwrap();
        node
    }

    fn config(
        backend_id: &str,
        max_concurrent: usize,
        max_queue_depth: usize,
    ) -> BackendAdmissionConfig {
        BackendAdmissionConfig {
            backend_id: backend_id.to_string(),
            max_concurrent,
            max_queue_depth,
            enabled: true,
            probe_status: "healthy".to_string(),
            config_fingerprint: format!("{backend_id}:{max_concurrent}:{max_queue_depth}"),
        }
    }

    fn request(request_id: &str) -> AgentRequest {
        AgentRequest {
            doc_id: format!("doc-{request_id}"),
            request_id: request_id.to_string(),
            agent_did: "did:defra-agent:test".to_string(),
            behavior_id: Some("default".to_string()),
            session_id: format!("session-{request_id}"),
            content: "hello".to_string(),
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            metadata: None,
            created_at: "2026-04-15T00:00:00Z".to_string(),
        }
    }

    async fn call_rows(node: &EmbeddedNode) -> Vec<Value> {
        let response = node
            .execute(
                r#"{
                    InferenceCall(order: { call_seq: ASC }) {
                        request_id
                        call_seq
                        backend_id
                        behavior_id
                        call_kind
                        call_state
                        failure_reason
                        queue_depth_at_enqueue
                    }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response
            .data
            .as_ref()
            .and_then(|data| data.get("InferenceCall"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn max_queue_depth_zero_allows_immediate_permit_and_rejects_saturated_backend() {
        let node = test_node().await;
        let registry = AdmissionRegistry::new(node.clone());
        registry.reconcile(
            1,
            &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 0))]),
        );
        let context =
            AdmissionCallContext::for_request(&request("req-zero"), "default", "backend-a");

        scope_request(context, async {
            let mut first = registry.acquire_current_call().await.unwrap();
            let error = match registry.acquire_current_call().await {
                Ok(_) => panic!("saturated backend should reject without queue capacity"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("QueueFull"));
            first.finish_success(None).await;
        })
        .await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_state"], "completed");
        assert_eq!(rows[1]["call_state"], "failed");
        assert_eq!(rows[1]["failure_reason"], "QueueFull");
    }

    #[tokio::test]
    async fn queued_calls_start_in_tokio_registration_order_after_permit_release() {
        let node = test_node().await;
        let registry = AdmissionRegistry::new(node.clone());
        registry.reconcile(
            1,
            &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 2))]),
        );
        let first_context =
            AdmissionCallContext::for_request(&request("req-ordered"), "default", "backend-a");
        let second_context = first_context.clone();

        scope_request(first_context, async {
            let mut first = registry.acquire_current_call().await.unwrap();
            let second_registry = registry.clone();
            let second = tokio::spawn(async move {
                scope_request(second_context, async move {
                    let mut permit = second_registry.acquire_current_call().await.unwrap();
                    permit.finish_success(None).await;
                })
                .await;
            });

            tokio::time::sleep(Duration::from_millis(50)).await;
            let rows = call_rows(node.as_ref()).await;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0]["call_state"], "running");
            assert_eq!(rows[1]["call_state"], "queued");

            first.finish_success(None).await;
            drop(first);
            second.await.unwrap();
        })
        .await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_state"], "completed");
        assert_eq!(rows[1]["call_state"], "completed");
        assert_eq!(rows[1]["queue_depth_at_enqueue"], 1);
    }

    #[tokio::test]
    async fn scoped_scheduled_calls_are_persisted_with_scheduled_kind() {
        let node = test_node().await;
        let registry = AdmissionRegistry::new(node.clone());
        registry.reconcile(
            1,
            &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
        );
        let context =
            AdmissionCallContext::for_request(&request("req-scheduled"), "default", "backend-a");

        scope_request(context, async {
            scope_call(CallKind::Scheduled, 1, async {
                let mut permit = registry.acquire_current_call().await.unwrap();
                permit.finish_success(None).await;
            })
            .await;
        })
        .await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["call_kind"], "scheduled");
        assert_eq!(rows[0]["call_state"], "completed");
    }

    #[tokio::test]
    async fn compaction_calls_share_backend_capacity_with_inference_calls() {
        let node = test_node().await;
        let registry = AdmissionRegistry::new(node.clone());
        registry.reconcile(
            1,
            &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
        );
        let inference_context =
            AdmissionCallContext::for_request(&request("req-compaction"), "default", "backend-a");
        let compaction_context = inference_context.clone();

        scope_request(inference_context, async {
            let mut inference = registry.acquire_current_call().await.unwrap();
            let compaction_registry = registry.clone();
            let compaction = tokio::spawn(async move {
                scope_request(compaction_context, async move {
                    scope_call(CallKind::Compaction, 1, async {
                        let mut permit = compaction_registry.acquire_current_call().await.unwrap();
                        permit.finish_success(None).await;
                    })
                    .await;
                })
                .await;
            });

            tokio::time::sleep(Duration::from_millis(50)).await;
            let rows = call_rows(node.as_ref()).await;
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0]["call_kind"], "inference");
            assert_eq!(rows[0]["call_state"], "running");
            assert_eq!(rows[1]["call_kind"], "compaction");
            assert_eq!(rows[1]["call_state"], "queued");

            inference.finish_success(None).await;
            drop(inference);
            compaction.await.unwrap();
        })
        .await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_state"], "completed");
        assert_eq!(rows[1]["call_state"], "completed");
        assert_eq!(rows[1]["queue_depth_at_enqueue"], 1);
    }
}
