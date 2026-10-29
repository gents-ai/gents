use std::sync::{Arc, Mutex};
use std::time::Duration;

use gents_loop::provider_activity::ActivityPhase;
use gents_loop::session_hook::ProviderObservation;
use rapidhash::RapidHashMap;
use serde::{Deserialize, Serialize};

pub const INFERENCE_ACTIVITY_PATH: &str = "/inference/activity";

const MAX_ACTIVE_OBSERVATIONS: usize = 256;
const SILENT_AFTER: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct InferenceKey {
    pub node: String,
    pub session: String,
    pub request: String,
    pub generation: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceActivity {
    AwaitingProvider,
    Waiting,
    Working,
    Retrying,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InferenceSnapshot {
    pub node: String,
    pub session: String,
    pub request: String,
    pub generation: String,
    pub turn: usize,
    pub attempt: u32,
    pub sequence: u64,
    pub activity: InferenceActivity,
}

struct Entry {
    turn: usize,
    attempt: u32,
    sequence: u64,
    observation: Option<ProviderObservation>,
    tools: RapidHashMap<String, std::time::Instant>,
}

#[derive(Clone, Default)]
pub struct InferenceObservationRegistry(Arc<Mutex<RapidHashMap<InferenceKey, Entry>>>);

pub(crate) struct InferenceObservationGuard {
    registry: InferenceObservationRegistry,
    key: InferenceKey,
}

impl InferenceObservationRegistry {
    pub(crate) fn begin(&self, key: InferenceKey) -> Option<InferenceObservationGuard> {
        let mut entries = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if entries.len() >= MAX_ACTIVE_OBSERVATIONS || entries.contains_key(&key) {
            return None;
        }
        entries.insert(
            key.clone(),
            Entry {
                turn: 0,
                attempt: 0,
                sequence: 0,
                observation: None,
                tools: Default::default(),
            },
        );
        Some(InferenceObservationGuard {
            registry: self.clone(),
            key,
        })
    }

    pub fn snapshot(&self, key: &InferenceKey) -> Option<InferenceSnapshot> {
        let mut entries = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let entry = entries.get_mut(key)?;
        let activity = if !entry.tools.is_empty() {
            if entry
                .tools
                .values()
                .any(|started| started.elapsed() < SILENT_AFTER)
            {
                InferenceActivity::Working
            } else {
                InferenceActivity::Waiting
            }
        } else {
            match entry.observation.as_ref() {
                None => InferenceActivity::AwaitingProvider,
                Some(ProviderObservation::Retrying { .. }) => InferenceActivity::Retrying,
                Some(ProviderObservation::Attempt { activity, .. }) => {
                    match activity.as_ref().map(|activity| activity.phase()) {
                        Some(ActivityPhase::Active(last)) if last.elapsed() < SILENT_AFTER => {
                            InferenceActivity::Working
                        }
                        _ => InferenceActivity::Waiting,
                    }
                }
            }
        };
        entry.sequence = entry.sequence.checked_add(1)?;
        Some(InferenceSnapshot {
            node: key.node.clone(),
            session: key.session.clone(),
            request: key.request.clone(),
            generation: key.generation.clone(),
            turn: entry.turn,
            attempt: entry.attempt,
            sequence: entry.sequence,
            activity,
        })
    }
}

pub(crate) struct ToolObservationGuard {
    owner: std::sync::Weak<InferenceObservationGuard>,
    id: String,
}

impl Drop for ToolObservationGuard {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut entries = owner.registry.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = entries.get_mut(&owner.key) {
            entry.tools.remove(&self.id);
        }
    }
}

impl InferenceObservationGuard {
    pub(crate) fn begin_tool(self: &Arc<Self>, id: &str) -> ToolObservationGuard {
        let mut entries = self.registry.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = entries.get_mut(&self.key) {
            entry.tools.insert(id.to_owned(), std::time::Instant::now());
        }
        ToolObservationGuard {
            owner: Arc::downgrade(self),
            id: id.to_owned(),
        }
    }

    pub(crate) fn observe(&self, observation: ProviderObservation) {
        let mut entries = self.registry.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = entries.get_mut(&self.key) else {
            return;
        };
        let (turn, attempt) = match &observation {
            ProviderObservation::Attempt { turn, attempt, .. }
            | ProviderObservation::Retrying { turn, attempt } => (*turn, *attempt),
        };
        if (turn, attempt) >= (entry.turn, entry.attempt) {
            entry.turn = turn;
            entry.attempt = attempt;
            entry.observation = Some(observation);
        }
    }
}

impl Drop for InferenceObservationGuard {
    fn drop(&mut self) {
        self.registry
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.key);
    }
}

impl crate::agent::Gents {
    pub async fn inference_observation_authorized(&self, actor: &str) -> anyhow::Result<bool> {
        if crate::agent::p2p_reconcile::enrollment_reconcile::runtime_observation_admitted(
            Some(actor),
            self.node_did(),
            None,
        ) {
            return Ok(true);
        }
        self.client_observation_authorized(actor).await
    }

    /// Returns only a currently claimed request visible to the authenticated actor.
    pub async fn read_inference_observation(
        &self,
        actor: identity::Did,
        key: &InferenceKey,
    ) -> anyhow::Result<Option<InferenceSnapshot>> {
        if key.node != self.node_did() {
            return Ok(None);
        }
        let visible = observation_visible(self.observation_node(), &actor, key).await?;
        Ok(visible
            .then(|| self.inference_observations().snapshot(key))
            .flatten())
    }
}

async fn observation_visible(
    node: &Arc<defra_node::EmbeddedNode>,
    actor: &identity::Did,
    key: &InferenceKey,
) -> anyhow::Result<bool> {
    let scope = if crate::plugin_job::control::operator_control_authorized(
        &key.node,
        &key.node,
        &key.node,
        Some(actor.as_ref()),
    ) {
        format!(
            r#"node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}"#,
            crate::graphql::escape_graphql_string(&key.node),
            crate::graphql::escape_graphql_string(&key.session)
        )
    } else {
        crate::session::session_scope_filter(&key.node, &key.session, Some(actor.as_ref()))
    };
    let request = crate::graphql::escape_graphql_string(&key.request);
    let generation = crate::graphql::escape_graphql_string(&key.generation);
    let query = format!(
        r#"{{ AgentSession(filter: {{{scope}}}, limit: 2) {{ _docID }} AgentRequest(filter: {{{scope}, request_id: {{_eq: "{request}"}}, execution_generation: {{_eq: "{generation}"}}}}, limit: 2) {{ _docID lifecycle_state }} }}"#
    );
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "inference_observation.authorize",
        |txn| {
            Box::pin(async {
                let response = txn.execute(&query).await?;
                let sessions = response
                    .pointer("/data/AgentSession")
                    .and_then(serde_json::Value::as_array);
                let requests = response
                    .pointer("/data/AgentRequest")
                    .and_then(serde_json::Value::as_array);
                let (Some(sessions), Some(requests)) = (sessions, requests) else {
                    return Ok(false);
                };
                if sessions.len() != 1 || requests.len() != 1 {
                    return Ok(false);
                }
                let Some(session_doc) = sessions[0]
                    .get("_docID")
                    .and_then(serde_json::Value::as_str)
                else {
                    return Ok(false);
                };
                let Some(request_doc) = requests[0]
                    .get("_docID")
                    .and_then(serde_json::Value::as_str)
                else {
                    return Ok(false);
                };
                let state = requests[0]
                    .get("lifecycle_state")
                    .and_then(serde_json::Value::as_str);
                if !matches!(
                    gents_protocol::request_lifecycle::RequestLifecycleState::parse_opt(state),
                    Some(
                        gents_protocol::request_lifecycle::RequestLifecycleState::Claimed
                            | gents_protocol::request_lifecycle::RequestLifecycleState::Processing
                    )
                ) {
                    return Ok(false);
                }
                let sessions = txn
                    .document_visibility_as(
                        actor.clone(),
                        "AgentSession",
                        &[session_doc.to_owned()],
                    )
                    .await?;
                let requests = txn
                    .document_visibility_as(
                        actor.clone(),
                        "AgentRequest",
                        &[request_doc.to_owned()],
                    )
                    .await?;
                Ok::<_, anyhow::Error>(
                    sessions.len() == 1
                        && sessions[0].visible
                        && requests.len() == 1
                        && requests[0].visible,
                )
            })
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matching_requester_still_requires_both_document_permissions() {
        use crate::NodeIdentity;
        let home = tempfile::tempdir().unwrap();
        let owner = crate::KeyIdentity::load_or_create(home.path().join("owner"), None).unwrap();
        let requester =
            crate::KeyIdentity::load_or_create(home.path().join("requester"), None).unwrap();
        let node = Arc::new(
            defra_node::EmbeddedNode::builder()
                .with_node_identity_did(owner.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        let policy = node
            .add_dac_policy(
                owner.did(),
                r#"
name: Activity read
resources:
  - name: documents
    relations:
      - name: reader
    permissions:
      - name: read
        expr: reader
      - name: update
      - name: delete
"#,
            )
            .await
            .unwrap();
        let access = crate::config_client::ConfigAccess::local(node.clone());
        for schema in [
            format!("type AgentSession @branchable @policy(id: \"{policy}\", resource: \"documents\") {{ node_did: String session_id: String requester_did: String }}"),
            format!("type AgentRequest @branchable @policy(id: \"{policy}\", resource: \"documents\") {{ node_did: String session_id: String requester_did: String request_id: String execution_generation: String lifecycle_state: String }}"),
        ] { access.add_collection(&schema).await.unwrap(); }
        let actor: identity::Did = requester.did().parse().unwrap();
        let owner_did: identity::Did = owner.did().parse().unwrap();
        let key = InferenceKey {
            node: "runtime".into(),
            session: "session".into(),
            request: "request".into(),
            generation: "generation".into(),
        };
        let requester_did = requester.did();
        let (session_doc, request_doc) = crate::config_client::ConfigAccess::transact_local_write(&node, Some(owner_did), "test.activity.acp", |txn| {
            Box::pin(async move {
                let session = txn.execute_with_variables("mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }", &serde_json::json!({"input":{"node_did":"runtime","session_id":"session","requester_did":requester_did}})).await?;
                let request = txn.execute_with_variables("mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }", &serde_json::json!({"input":{"node_did":"runtime","session_id":"session","requester_did":requester_did,"request_id":"request","execution_generation":"generation","lifecycle_state":"processing"}})).await?;
                Ok::<_, anyhow::Error>((crate::graphql::created_doc_id(&session, "AgentSession")?, crate::graphql::created_doc_id(&request, "AgentRequest")?))
            })
        }).await.unwrap();
        assert!(!observation_visible(&node, &actor, &key).await.unwrap());
        node.add_dac_actor_relationship(
            owner.did(),
            "AgentSession",
            &session_doc,
            "reader",
            requester.did(),
        )
        .await
        .unwrap();
        assert!(!observation_visible(&node, &actor, &key).await.unwrap());
        node.add_dac_actor_relationship(
            owner.did(),
            "AgentRequest",
            &request_doc,
            "reader",
            requester.did(),
        )
        .await
        .unwrap();
        assert!(observation_visible(&node, &actor, &key).await.unwrap());
        node.shutdown().await;
    }

    fn key() -> InferenceKey {
        InferenceKey {
            node: "node".into(),
            session: "session".into(),
            request: "request".into(),
            generation: "generation".into(),
        }
    }

    #[test]
    fn initial_observation_matches_modeled_awaiting_provider() {
        let registry = InferenceObservationRegistry::default();
        let _guard = registry.begin(key()).unwrap();
        let snapshot = registry.snapshot(&key()).unwrap();
        let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        assert_eq!(
            serde_json::to_value(snapshot.activity).unwrap(),
            contract["inference_observation_cases"][0]["expected"][0]
        );
    }

    #[test]
    fn retry_and_turn_observations_are_fenced_and_removed_on_drop() {
        let registry = InferenceObservationRegistry::default();
        let guard = registry.begin(key()).unwrap();
        let activity = Arc::new(gents_loop::provider_activity::ProviderActivity::default());
        activity.touch();
        guard.observe(ProviderObservation::Attempt {
            turn: 0,
            attempt: 0,
            activity: Some(activity),
        });
        let first = registry.snapshot(&key()).unwrap();
        assert!(matches!(first.activity, InferenceActivity::Working));
        guard.observe(ProviderObservation::Retrying {
            turn: 0,
            attempt: 0,
        });
        let retry = registry.snapshot(&key()).unwrap();
        assert!(matches!(retry.activity, InferenceActivity::Retrying));
        assert!(retry.sequence > first.sequence);
        guard.observe(ProviderObservation::Attempt {
            turn: 1,
            attempt: 0,
            activity: None,
        });
        guard.observe(ProviderObservation::Retrying {
            turn: 0,
            attempt: 4,
        });
        let next = registry.snapshot(&key()).unwrap();
        assert_eq!((next.turn, next.attempt), (1, 0));
        assert!(matches!(next.activity, InferenceActivity::Waiting));
        drop(guard);
        assert!(registry.snapshot(&key()).is_none());
    }

    #[test]
    fn concurrent_tool_activity_is_removed_per_call() {
        let registry = InferenceObservationRegistry::default();
        let guard = Arc::new(registry.begin(key()).unwrap());
        let first = guard.begin_tool("first");
        let second = guard.begin_tool("second");
        drop(first);
        assert!(matches!(
            registry.snapshot(&key()).unwrap().activity,
            InferenceActivity::Working
        ));
        drop(second);
        assert!(matches!(
            registry.snapshot(&key()).unwrap().activity,
            InferenceActivity::AwaitingProvider
        ));
        drop(guard);
        assert!(registry.snapshot(&key()).is_none());
    }

    #[test]
    fn handed_off_tool_does_not_keep_request_or_remove_replacement_activity() {
        let registry = InferenceObservationRegistry::default();
        let request = Arc::new(registry.begin(key()).unwrap());
        let handed_off = request.begin_tool("call");
        drop(request);
        assert!(registry.snapshot(&key()).is_none());

        let next = Arc::new(registry.begin(key()).unwrap());
        let active = next.begin_tool("call");
        drop(handed_off);
        assert!(matches!(
            registry.snapshot(&key()).unwrap().activity,
            InferenceActivity::Working
        ));
        drop(active);
        drop(next);
    }
}
