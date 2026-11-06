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

impl crate::runtime::agent::Gents {
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
    node: &Arc<defra_node::Node>,
    actor: &identity::Did,
    key: &InferenceKey,
) -> anyhow::Result<bool> {
    let scope = format!(
        r#"node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}"#,
        crate::store::graphql::escape_graphql_string(&key.node),
        crate::store::graphql::escape_graphql_string(&key.session)
    );
    let request = crate::store::graphql::escape_graphql_string(&key.request);
    let generation = crate::store::graphql::escape_graphql_string(&key.generation);
    let query = format!(
        r#"{{ AgentSession(filter: {{{scope}}}, limit: 2) {{ _docID }} AgentRequest(filter: {{{scope}, request_id: {{_eq: "{request}"}}, execution_generation: {{_eq: "{generation}"}}}}, limit: 2) {{ _docID lifecycle_state }} }}"#
    );
    crate::store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(actor.clone()),
        "inference_observation.authorize",
        |txn| {
            Box::pin(async {
                let response = txn.execute(&query).await?;
                Ok::<_, anyhow::Error>(activity_visible(&response))
            })
        },
    )
    .await
}

fn activity_visible(response: &serde_json::Value) -> bool {
    let sessions = response
        .pointer("/data/AgentSession")
        .and_then(serde_json::Value::as_array);
    let requests = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array);
    let (Some([session]), Some([request])) =
        (sessions.map(Vec::as_slice), requests.map(Vec::as_slice))
    else {
        return false;
    };
    if session
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .is_none()
        || request
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .is_none()
    {
        return false;
    }
    let state = request
        .get("lifecycle_state")
        .and_then(serde_json::Value::as_str);
    matches!(
        gents_protocol::request_lifecycle::RequestLifecycleState::parse_opt(state),
        Some(
            gents_protocol::request_lifecycle::RequestLifecycleState::Claimed
                | gents_protocol::request_lifecycle::RequestLifecycleState::Processing
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_matches_lean_for_direct_reads() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().inference_visibility_cases;
        for case in cases {
            let sessions = case["sessions"].as_u64().unwrap() as usize;
            let requests = case["requests"].as_u64().unwrap() as usize;
            let state = if case["active"].as_bool().unwrap() {
                "processing"
            } else {
                "completed"
            };
            let response = serde_json::json!({"data": {
                "AgentSession": vec![serde_json::json!({"_docID":"session"}); sessions],
                "AgentRequest": vec![serde_json::json!({"_docID":"request", "lifecycle_state":state}); requests],
            }});
            assert_eq!(
                activity_visible(&response),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    #[tokio::test]
    async fn delegated_reader_requires_both_document_permissions() {
        use crate::NodeIdentity;
        let home = tempfile::tempdir().unwrap();
        let owner = crate::KeyIdentity::load_or_create(home.path().join("owner"), None).unwrap();
        let requester =
            crate::KeyIdentity::load_or_create(home.path().join("requester"), None).unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(owner.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        let policy = node
            .add_dac_policy_as(
                &defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned())),
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
        let access = crate::store::config_client::ConfigAccess::local(node.clone());
        for schema in [
            format!("type AgentSession @branchable @policy(id: \"{policy}\", resource: \"documents\") {{ node_did: String session_id: String requester_did: String }}"),
            format!("type AgentRequest @branchable @policy(id: \"{policy}\", resource: \"documents\") {{ node_did: String session_id: String requester_did: String request_id: String execution_generation: String lifecycle_state: String }}"),
        ] { access.add_collection(&schema).await.unwrap(); }
        let actor: identity::Did = requester.did().parse().unwrap();
        let owner_did: identity::Did = owner.did().parse().unwrap();
        crate::store::config_client::ConfigAccess::local_as(node.clone(), owner_did.clone())
            .grant_node_permission(&actor, acp::nac::NodePermission::DocumentRead)
            .await
            .unwrap();
        let key = InferenceKey {
            node: "runtime".into(),
            session: "session".into(),
            request: "request".into(),
            generation: "generation".into(),
        };
        let requester_did = owner.did();
        let (session_doc, request_doc) = crate::store::config_client::ConfigAccess::transact_local_write(&node, Some(owner_did), "test.activity.acp", |txn| {
            Box::pin(async move {
                let session = txn.execute_with_variables("mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }", &serde_json::json!({"input":{"node_did":"runtime","session_id":"session","requester_did":requester_did}})).await?;
                let request = txn.execute_with_variables("mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }", &serde_json::json!({"input":{"node_did":"runtime","session_id":"session","requester_did":requester_did,"request_id":"request","execution_generation":"generation","lifecycle_state":"processing"}})).await?;
                Ok::<_, anyhow::Error>((crate::store::graphql::created_doc_id(&session, "AgentSession")?, crate::store::graphql::created_doc_id(&request, "AgentRequest")?))
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
        node.shutdown().await.expect("node shutdown");
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
