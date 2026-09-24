use anyhow::{bail, Context, Result};
use defra_p2p_adapter::P2pDocumentRequest;
use gents::background_completion::{
    observe_cancel_cascade_ack, project_background_subagent_completion, CancelAckOutcome,
    STUCK_CANCEL_THRESHOLD_SECS,
};
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn as read_desired_state_record,
    ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{CancelCause, CascadeDispatch, ToolCallLifecycle};
use gents::{Collection, DocumentRuntimeOptions, Gents, RequestLifecycle, ToolCeiling};
use gents_protocol::output::{OutputSource, TerminalOutput};
use gents_protocol::request_input::QueueSource;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnSpec,
};
use crate::support::enrollment::{authorize_enrollment_peer, wait_for_peer_identity};
use crate::support::fixtures::{
    bind_behavior_backend, bind_default_behavior_backend, configure_subagent_behavior,
    subagent_target,
};
use crate::support::interrupt::BootedAgent;
use crate::support::native_remote_spawn::{wait_for_bridge, wait_for_child};
use crate::support::p2p_waits::{wait_for_connected_peer, wait_for_listen_addr};
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{first_optional_row, test_p2p_db, TestDb};

use super::scenario::{
    ModeledAction, ModeledCancelAckEvent, ModeledCancelAckOutcome, ModeledRecoveryCheckpoint,
    ModeledScenario, NodeId,
};

pub struct HarnessNode {
    pub id: NodeId,
    pub db: TestDb,
}

impl HarnessNode {
    fn did(&self) -> &str {
        self.db.node_identity.did()
    }
}

pub struct Harness {
    a: HarnessNode,
    b: HarnessNode,
    history: Vec<Observation>,
    generated_bridges: HashMap<String, GeneratedBridge>,
    generated_pairing_done: bool,
    generated_child_backend: Option<MockStreamingBackend>,
    generated_child_agent: Option<BootedAgent>,
    child_lease_secs: u64,
    child_backend_capacity: usize,
    modeled_generations: HashMap<String, HashMap<u64, String>>,
    observed_cancel_ack_events: Vec<ModeledCancelAckEvent>,
}

struct GeneratedBridge {
    symbolic_child: String,
    session_id: String,
    physical_child_request_id: Option<String>,
    physical_bridge_doc_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub a_bridge_rows: Vec<BridgeObservation>,
    pub b_bridge_rows: Vec<BridgeObservation>,
    pub a_rejected_spawn_invocation_ids: Vec<String>,
    pub a_child_requests: Vec<RequestObservation>,
    pub b_child_requests: Vec<RequestObservation>,
    pub subagent_notifications: Vec<String>,
    pub background_wakeup_keys: Vec<String>,
    pub a_process_generation: u64,
    pub b_process_generation: u64,
    pub crashed_node: Option<NodeId>,
}

impl Observation {
    pub fn child_for_bridge(&self, bridge: &BridgeObservation) -> Option<&RequestObservation> {
        let child_id = bridge.child_request_id.as_ref()?;
        self.a_child_requests
            .iter()
            .chain(self.b_child_requests.iter())
            .find(|child| &child.request_id == child_id)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeObservation {
    pub request_id: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub lifecycle_state: String,
    pub child_request_id: Option<String>,
    pub cancel_cause: Option<String>,
    pub cancel_cascade_intent_at: Option<String>,
    pub cancel_pending_remote_ack: Option<bool>,
    pub stuck_since: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequestObservation {
    pub request_id: String,
    pub agent_did: String,
    pub lifecycle_state: RequestLifecycleState,
    pub caused_by_parent_tool_call_id: Option<String>,
    pub interrupt_requested_at: Option<String>,
}

impl RequestObservation {
    pub fn is_terminal(&self) -> bool {
        self.lifecycle_state.is_terminal()
    }
}

impl Harness {
    /// Start the generated R5 runner with real transport. Do not subscribe to
    /// the modeled collections' live P2P topics: their arrival is controlled
    /// by the explicit exact-document push actions below.
    pub async fn start_two_p2p_nodes() -> Result<Self> {
        let a = HarnessNode {
            id: "A".to_string(),
            db: test_p2p_db("r5-generated-a").await,
        };
        let b = HarnessNode {
            id: "B".to_string(),
            db: test_p2p_db("r5-generated-b").await,
        };
        let b_address = wait_for_listen_addr(b.db.node.as_ref()).await;
        let a_address = wait_for_listen_addr(a.db.node.as_ref()).await;
        a.db.node
            .p2p()
            .context("R5 A has no P2P transport")?
            .connect_peer(&b_address)
            .await
            .context("connect R5 generated peers")?;
        wait_for_connected_peer(a.db.node.as_ref()).await;
        wait_for_connected_peer(b.db.node.as_ref()).await;
        // Defra's exact-doc push uses the existing replicator retry guard and
        // otherwise returns Ok without sending. Register a route over only
        // AgentNetwork, never the four modeled R5 collections, so their
        // arrival remains controlled by the explicit per-action push.
        for (from, address) in [(&a, &b_address), (&b, &a_address)] {
            from.db
                .node
                .p2p()
                .context("R5 generated peer has no P2P transport")?
                .add_replicator(
                    vec!["AgentNetwork".to_string()],
                    Some(address),
                    Default::default(),
                    Vec::new(),
                    None,
                )
                .await
                .context("register exact-document push route")?;
        }
        let mut harness = Self {
            a,
            b,
            history: Vec::new(),
            generated_bridges: HashMap::new(),
            generated_pairing_done: false,
            generated_child_backend: None,
            generated_child_agent: None,
            child_lease_secs: 0,
            child_backend_capacity: 0,
            modeled_generations: HashMap::new(),
            observed_cancel_ack_events: Vec::new(),
        };
        harness.record_observation().await?;
        Ok(harness)
    }

    pub async fn start_generated(scenario: &ModeledScenario) -> Result<Self> {
        let mut harness = Self::start_two_p2p_nodes().await?;
        anyhow::ensure!(
            scenario.child_lease_secs > 0,
            "R5 modeled child lease must be positive"
        );
        anyhow::ensure!(
            scenario.cancel_ack_threshold_secs == u64::try_from(STUCK_CANCEL_THRESHOLD_SECS)?,
            "R5 modeled cancel-ack threshold differs from native owner"
        );
        harness.child_lease_secs = scenario.child_lease_secs;
        harness.child_backend_capacity = scenario
            .actions
            .iter()
            .filter(|action| {
                matches!(
                    action,
                    ModeledAction::PublishAcceptedBackgroundBridge { .. }
                )
            })
            .count()
            .max(1);
        let plans = scenario
            .actions
            .iter()
            .filter_map(|action| match action {
                ModeledAction::PublishAcceptedBackgroundBridge { child, .. } => Some(child),
                _ => None,
            })
            .map(|child| {
                let prompt = format!("R5 child {child}");
                StreamPlan::current_authored_user(
                    &prompt,
                    vec![StreamResponse::Stream(StreamScript::paused_before(
                        &prompt,
                        vec![StreamChunk::text(format!("R5 result {child}"))],
                    ))],
                )
            })
            .collect();
        let backend = MockStreamingBackend::start_with_plans("r5-generated-child-model", plans)?;
        bind_default_behavior_backend(
            harness.b.db.node.as_ref(),
            harness.b.did(),
            "r5-generated-default-backend",
            backend.endpoint(),
        )
        .await;
        harness.generated_child_backend = Some(backend);
        Ok(harness)
    }

    /// Transfer an exact persisted document ID through DefraDB's real P2P
    /// endpoint. Never recreate a recipient row or change physical identity.
    async fn push_document(
        &self,
        source: &NodeId,
        target: &NodeId,
        collection: &str,
        doc_id: &str,
    ) -> Result<()> {
        anyhow::ensure!(source != target, "R5 replication endpoints must differ");
        let from = self.node(source)?;
        let to = self.node(target)?;
        ensure_physical_document(from, collection, doc_id).await?;
        let (peer_id, _) = wait_for_peer_identity(to.db.node.as_ref()).await;
        from.db
            .node
            .p2p()
            .context("R5 replication source has no P2P transport")?
            .push_documents_to_peer(
                &peer_id,
                vec![P2pDocumentRequest {
                    collection: collection.to_string(),
                    doc_id: doc_id.to_string(),
                }],
            )
            .await
            .with_context(|| format!("push {collection}/{doc_id} to {target}"))?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if physical_document_exists(to, collection, doc_id).await? {
                return Ok(());
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "P2P push did not materialize {collection}/{doc_id} on {target}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn pair_generated_principals(&mut self, node: &NodeId, peer: &NodeId) -> Result<()> {
        match (node.as_str(), peer.as_str()) {
            // The transport connection is already physical in the generated
            // fixture; the first modeled pair observes it without adding a
            // continuous replicator that would collapse the trace windows.
            ("A", "B") => {
                wait_for_connected_peer(self.a.db.node.as_ref()).await;
                Ok(())
            }
            // Enrollment is the production owner of the durable route from
            // parent A to child B. It uses the actual node identities.
            ("B", "A") => {
                if self.generated_pairing_done {
                    return Ok(());
                }
                let (a_peer, a_address) = wait_for_peer_identity(self.a.db.node.as_ref()).await;
                // This script models each transcript transfer as an explicit
                // P2P push. Own the base route before enrollment reconciliation
                // so its broad client template cannot subscribe to those
                // documents between modeled actions. The config-only overlay
                // still resolves through the signed enrollment endpoint; it
                // grants neither identity nor access on its own.
                write_generated_scripted_route(&self.b, &a_peer, &a_address).await?;
                authorize_enrollment_peer(
                    self.b.db.node.clone(),
                    "r5-generated-route",
                    "R5 generated route",
                    self.b.db.node_identity.clone(),
                    self.a.db.node_identity.clone(),
                    &a_peer,
                    &a_address,
                )
                .await;
                self.generated_pairing_done = true;
                Ok(())
            }
            _ => bail!("R5 model paired unsupported direction {node} -> {peer}"),
        }
    }

    async fn publish_accepted_background_bridge(
        &mut self,
        tool: &str,
        child: &str,
        session: &str,
        parent_depth: u32,
        expect_depth_rejection: bool,
    ) -> Result<()> {
        anyhow::ensure!(
            (parent_depth >= gents::tool_call_lifecycle::MAX_SUBAGENT_DEPTH)
                == expect_depth_rejection,
            "modeled R5 spawn outcome does not match the owned depth ceiling"
        );
        anyhow::ensure!(
            !self.generated_bridges.contains_key(tool),
            "duplicate R5 accepted bridge {tool}"
        );
        if expect_depth_rejection {
            return self
                .publish_depth_rejected_spawn_invocation(tool, child, session, parent_depth)
                .await;
        }
        const CHILD_BEHAVIOR: &str = "r5-generated-child-behavior";
        configure_subagent_behavior(
            self.b.db.node.as_ref(),
            self.b.did(),
            CHILD_BEHAVIOR,
            "r5-generated-child-tools",
            Vec::new(),
            true,
            true,
            Some(true),
        )
        .await;
        // AgentSession selects one behavior for its lifetime. Multi-tool
        // scenarios intentionally share a parent session, so their successive
        // requests must use that same selected behavior.
        let parent_behavior = format!("r5-generated-parent-session-{session}");
        configure_subagent_behavior(
            self.a.db.node.as_ref(),
            self.a.did(),
            &parent_behavior,
            &format!("r5-generated-parent-tools-{tool}"),
            vec![subagent_target(
                self.a.did(),
                CHILD_BEHAVIOR.to_string(),
                self.b.did().to_string(),
                CHILD_BEHAVIOR.to_string(),
            )],
            true,
            true,
            Some(true),
        )
        .await;
        let parent_request = format!("r5-generated-parent-request-{tool}");
        let backend_id = format!("r5-generated-parent-backend-{tool}");
        let prompt = format!("accepted R5 background bridge {tool}");
        let arguments = json!({
            "name": CHILD_BEHAVIOR,
            "prompt": format!("R5 child {child}"),
            "await_mode": "background",
        })
        .to_string();
        let configured = [parent_behavior.as_str()];
        let prepared = prepare_accepted_turn(
            &self.a.db,
            AcceptedTurnSpec {
                backend_id: &backend_id,
                model: "r5-generated-parent-model",
                parent_behavior_id: &parent_behavior,
                configured_behavior_ids: &configured,
                request_id: &parent_request,
                session_id: session,
                prompt: &prompt,
                accepted_chunks: vec![StreamChunk::tool_call(tool, "spawn_subagent", arguments)],
                child_plans: Vec::new(),
                valid_until: None,
                subagent_depth: Some(parent_depth),
                request_setup: None,
            },
        )
        .await;
        let agent = Gents::from_default_behavior_documents(
            self.a.db.node.clone(),
            self.a.db.node_identity.clone(),
            DocumentRuntimeOptions {
                tool_ceiling: ToolCeiling::meta_only(),
                ..Default::default()
            },
        )
        .await?;
        let runtime = boot_prepared_accepted_turn(&self.a.db, prepared, agent).await;
        crate::support::live_inference::wait_for_request_terminal(
            self.a.db.node.as_ref(),
            &parent_request,
            std::time::Duration::from_secs(20),
        )
        .await;
        let request_id = escape_graphql_string(&parent_request);
        let observed = self
            .a
            .db
            .node
            .execute(&format!(
                r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 2) {{
                _docID lifecycle_state failure_reason
            }}
            AgentToolCall(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 2) {{
                _docID tool_call_id lifecycle_state child_request_id
            }}
        }}"#
            ))
            .await;
        anyhow::ensure!(
            !observed.has_errors(),
            "R5 accepted bridge parent observation failed: {:?}",
            observed.errors
        );
        let tools = observed
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .context("R5 accepted bridge parent query omitted tool rows")?;
        anyhow::ensure!(
            tools.len() == 1 && tools[0]["tool_call_id"] == tool,
            "R5 accepted bridge was not published with its parent request: {:?}",
            observed.data
        );
        let bridge = wait_for_bridge(self.a.db.node.as_ref(), session, tool).await;
        runtime.shutdown().await;
        self.generated_bridges.insert(
            tool.to_string(),
            GeneratedBridge {
                symbolic_child: child.to_string(),
                session_id: session.to_string(),
                physical_child_request_id: bridge.child_request_id,
                physical_bridge_doc_id: bridge.doc_id,
            },
        );
        Ok(())
    }

    /// Reach the modeled depth ceiling through three actual local-child
    /// admissions. A local-self request stamped `subagent_depth = 3` is
    /// rejected at ingest, before the provider invocation this case tests.
    async fn publish_depth_rejected_spawn_invocation(
        &mut self,
        tool: &str,
        child: &str,
        session: &str,
        parent_depth: u32,
    ) -> Result<()> {
        anyhow::ensure!(
            parent_depth == gents::tool_call_lifecycle::MAX_SUBAGENT_DEPTH,
            "R5 depth rejection fixture must reach the modeled ceiling exactly"
        );
        let root_behavior = format!("r5-depth-root-{session}");
        let child_behavior = format!("r5-depth-child-{session}");
        let target = subagent_target(
            self.a.did(),
            child_behavior.clone(),
            self.a.did().to_string(),
            child_behavior.clone(),
        );
        // The target validator requires the destination Behavior to exist
        // before the recursive self-target can be attached.
        configure_subagent_behavior(
            self.a.db.node.as_ref(),
            self.a.did(),
            &child_behavior,
            &format!("{child_behavior}-tools"),
            Vec::new(),
            true,
            true,
            Some(true),
        )
        .await;
        for (behavior, tools_id) in [
            (child_behavior.as_str(), format!("{child_behavior}-tools")),
            (root_behavior.as_str(), format!("{root_behavior}-tools")),
        ] {
            configure_subagent_behavior(
                self.a.db.node.as_ref(),
                self.a.did(),
                behavior,
                &tools_id,
                vec![target.clone()],
                true,
                true,
                Some(true),
            )
            .await;
        }

        let root_request = format!("r5-depth-root-request-{tool}");
        let root_prompt = format!("R5 depth root {tool}");
        let descendant_prompts: Vec<_> = (1..=parent_depth)
            .map(|depth| format!("R5 depth child {tool} {depth}"))
            .collect();
        let ancestor_tool = |depth: u32| format!("r5-depth-ancestor-{tool}-{depth}");
        let spawn_chunks = |call_id: String, prompt: String, await_mode: &str| {
            vec![StreamChunk::tool_call(
                call_id,
                "spawn_subagent",
                json!({
                    "name": child_behavior,
                    "prompt": prompt,
                    "await_mode": await_mode,
                })
                .to_string(),
            )]
        };
        let mut descendant_plans = Vec::new();
        for depth in 1..=parent_depth {
            let prompt = descendant_prompts[(depth - 1) as usize].clone();
            let chunks = if depth == parent_depth {
                spawn_chunks(
                    tool.to_string(),
                    format!("R5 rejected child {child}"),
                    "background",
                )
            } else {
                spawn_chunks(
                    ancestor_tool(depth),
                    descendant_prompts[depth as usize].clone(),
                    "background",
                )
            };
            descendant_plans.push(StreamPlan::current_authored_user(
                prompt.clone(),
                vec![
                    StreamResponse::streams(prompt.clone(), chunks),
                    StreamResponse::completes(prompt, ["depth turn complete"]),
                ],
            ));
        }
        let configured = [root_behavior.as_str(), child_behavior.as_str()];
        let backend_id = format!("r5-depth-backend-{tool}");
        let prepared = prepare_accepted_turn(
            &self.a.db,
            AcceptedTurnSpec {
                backend_id: &backend_id,
                model: "r5-generated-parent-model",
                parent_behavior_id: &root_behavior,
                configured_behavior_ids: &configured,
                request_id: &root_request,
                session_id: session,
                prompt: &root_prompt,
                accepted_chunks: spawn_chunks(
                    ancestor_tool(0),
                    descendant_prompts[0].clone(),
                    "background",
                ),
                child_plans: descendant_plans,
                valid_until: None,
                subagent_depth: Some(0),
                request_setup: None,
            },
        )
        .await;
        let agent = Gents::from_default_behavior_documents(
            self.a.db.node.clone(),
            self.a.db.node_identity.clone(),
            DocumentRuntimeOptions {
                tool_ceiling: ToolCeiling::meta_only(),
                ..Default::default()
            },
        )
        .await?;
        let runtime = boot_prepared_accepted_turn(&self.a.db, prepared, agent).await;
        let mut parent_request = root_request;
        for depth in 0..parent_depth {
            let parent = load_request(&self.a, &parent_request).await?;
            let parent_tool = wait_for_bridge(
                self.a.db.node.as_ref(),
                &parent.session_id,
                &ancestor_tool(depth),
            )
            .await;
            anyhow::ensure!(
                parent_tool.request_id == parent.request_id
                    && parent_tool.request_doc_id == parent.doc_id,
                "R5 depth ancestor bridge has wrong physical parent at depth {depth}"
            );
            let child_request_id = parent_tool
                .child_request_id
                .as_deref()
                .context("R5 depth ancestor did not reserve a child")?;
            let descendant = wait_for_child(self.a.db.node.as_ref(), child_request_id).await;
            anyhow::ensure!(
                descendant.subagent_depth == Some(depth + 1)
                    && descendant.agent_did.as_deref() == Some(self.a.did())
                    && descendant.caused_by_parent_request_id.as_deref()
                        == Some(parent.request_id.as_str())
                    && descendant.caused_by_parent_request_doc_id.as_deref()
                        == Some(parent.doc_id.as_str())
                    && descendant.caused_by_parent_tool_call_id.as_deref()
                        == Some(parent_tool.tool_call_id.as_str())
                    && descendant.caused_by_parent_tool_call_doc_id.as_deref()
                        == Some(parent_tool.doc_id.as_str()),
                "R5 depth ancestor {depth} has incoherent durable lineage: {descendant:?}"
            );
            parent_request = descendant.request_id;
        }
        crate::support::live_inference::wait_for_request_terminal(
            self.a.db.node.as_ref(),
            &parent_request,
            std::time::Duration::from_secs(20),
        )
        .await;
        let escaped = escape_graphql_string(&parent_request);
        let observed = self
            .a
            .db
            .node
            .execute(&format!(
                r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                    _docID lifecycle_state failure_reason subagent_depth admission_kind
                }}
                AgentToolCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                    _docID tool_call_id tool_name lifecycle_state tool_failure_class
                    await_mode child_request_id
                }}
            }}"#
            ))
            .await;
        anyhow::ensure!(
            !observed.has_errors(),
            "R5 max-depth invocation observation failed: {:?}",
            observed.errors
        );
        let rows = observed
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .context("R5 max-depth invocation query omitted tool rows")?;
        anyhow::ensure!(
            rows.len() == 1
                && rows[0]["tool_call_id"] == tool
                && rows[0]["tool_name"] == "spawn_subagent"
                && rows[0]["lifecycle_state"] == "failed"
                && rows[0]["tool_failure_class"] == "argumentInvalid"
                && rows[0]["await_mode"] == "foreground"
                && rows[0]["child_request_id"].is_null(),
            "R5 max-depth invocation did not persist the modeled failed foreground tool without a child: {:?}",
            observed.data
        );
        anyhow::ensure!(
            !load_child_requests(&self.b)
                .await?
                .iter()
                .any(|row| row.request_id == child),
            "R5 max-depth rejection materialized a remote child"
        );
        runtime.shutdown().await;
        Ok(())
    }

    fn generated_bridge(&self, tool: &str, child: &str) -> Result<&GeneratedBridge> {
        let bridge = self
            .generated_bridges
            .get(tool)
            .with_context(|| format!("R5 bridge {tool} was not accepted"))?;
        anyhow::ensure!(
            bridge.symbolic_child == child,
            "R5 bridge {tool} belongs to {}, not {child}",
            bridge.symbolic_child
        );
        Ok(bridge)
    }

    async fn replicate_generated_bridge(
        &self,
        tool: &str,
        source: &NodeId,
        target: &NodeId,
        first_arrival: bool,
    ) -> Result<()> {
        let bridge = self
            .generated_bridges
            .get(tool)
            .with_context(|| format!("R5 bridge {tool} was not accepted"))?;
        if first_arrival {
            anyhow::ensure!(
                !physical_document_exists(
                    self.node(target)?,
                    "AgentToolCall",
                    &bridge.physical_bridge_doc_id
                )
                .await?,
                "R5 bridge {tool} arrived before modeled ReplicateBridge"
            );
        }
        self.push_document(
            source,
            target,
            "AgentToolCall",
            &bridge.physical_bridge_doc_id,
        )
        .await?;
        if first_arrival && source == "A" && target == "B" {
            // B may consume the bounded delegated input, but the direct
            // lifecycle reader must refuse to reconstruct A's parent output
            // even though the physical bridge was delivered to B.
            let outcome =
                ToolCallLifecycle::load(self.b.db.node.clone(), &bridge.session_id, tool).await;
            anyhow::ensure!(
                outcome.is_err_and(|error| {
                    error
                        .to_string()
                        .contains("foreign node cannot reconstruct delegated parent output")
                }),
                "remote host direct admission did not reject parent output reconstruction"
            );
        }
        Ok(())
    }

    async fn materialize_generated_child(&mut self, child: &str, tool: &str) -> Result<()> {
        let physical_child = self
            .generated_bridge(tool, child)?
            .physical_child_request_id
            .clone();
        let Some(physical_child) = physical_child else {
            // An accepted tool row may have no child reservation when the
            // production admission owner rejects delegation (depth ceiling).
            return Ok(());
        };
        if self.generated_child_agent.is_none() {
            let backend = self
                .generated_child_backend
                .as_ref()
                .context("generated R5 child provider backend was not prepared")?;
            bind_behavior_backend(
                self.b.db.node.as_ref(),
                self.b.did(),
                "r5-generated-child-behavior",
                "r5-generated-child-backend",
                backend.endpoint(),
                "r5-generated-child-model",
            )
            .await;
            configure_generated_child_execution(
                &self.b,
                self.child_lease_secs,
                self.child_backend_capacity,
            )
            .await?;
            let agent = Gents::from_default_behavior_documents(
                self.b.db.node.clone(),
                self.b.db.node_identity.clone(),
                DocumentRuntimeOptions {
                    tool_ceiling: ToolCeiling::meta_only(),
                    ..Default::default()
                },
            )
            .await?;
            let did = agent.agent_did().to_string();
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let handle = tokio::spawn(agent.run(shutdown_rx));
            crate::support::interrupt::wait_for_runtime_ready(self.b.db.node.as_ref(), &did).await;
            self.generated_child_agent = Some(BootedAgent::new(shutdown_tx, handle, did));
        }
        let observed = wait_for_child(self.b.db.node.as_ref(), &physical_child).await;
        anyhow::ensure!(
            observed.caused_by_parent_tool_call_id.as_deref() == Some(tool),
            "R5 child {} was not materialized from bridge {tool}",
            observed.request_id
        );
        anyhow::ensure!(
            observed.requester_did.as_deref() == Some(self.a.did()),
            "R5 cross-principal child {} must route its output to coordinator {}, observed requester {:?}",
            observed.request_id,
            self.a.did(),
            observed.requester_did
        );
        Ok(())
    }

    async fn wait_for_generated_child_execution_before_b_crash(&self) -> Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        for bridge in self.generated_bridges.values() {
            let Some(physical_child) = bridge.physical_child_request_id.as_deref() else {
                continue;
            };
            loop {
                let row = crate::support::load_request_row_by_logical_id(
                    self.b.db.node.as_ref(),
                    physical_child,
                )
                .await;
                if row.lifecycle_state == Some(RequestLifecycleState::Processing)
                    && row.execution_lease_expires_at.is_some()
                {
                    break;
                }
                anyhow::ensure!(
                    tokio::time::Instant::now() < deadline,
                    "R5 B crash did not occur mid-execution for child {}: state={:?}, failure={:?}, lease={:?}",
                    bridge.symbolic_child,
                    row.lifecycle_state,
                    row.failure_reason,
                    row.execution_lease_expires_at
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        Ok(())
    }

    async fn replicate_generated_child(
        &self,
        child: &str,
        source: &NodeId,
        target: &NodeId,
        first_arrival: bool,
    ) -> Result<()> {
        let bridge = self
            .generated_bridges
            .values()
            .find(|bridge| bridge.symbolic_child == child)
            .with_context(|| format!("R5 child {child} has no accepted bridge"))?;
        let physical_child = bridge
            .physical_child_request_id
            .as_deref()
            .with_context(|| format!("R5 child {child} was not reserved"))?;
        let from = self.node(source)?;
        let request = wait_for_child(from.db.node.as_ref(), physical_child).await;
        if first_arrival {
            anyhow::ensure!(
                !physical_document_exists(self.node(target)?, "AgentRequest", &request.doc_id)
                    .await?,
                "R5 child {child} arrived before modeled ReplicateChild"
            );
        } else {
            let target_row = crate::support::load_request_row_by_logical_id(
                self.node(target)?.db.node.as_ref(),
                physical_child,
            )
            .await;
            anyhow::ensure!(
                !target_row
                    .lifecycle_state
                    .is_some_and(|state| state.is_terminal()),
                "R5 child {child} terminal arrived before modeled ReplicateTerminalRequest"
            );
        }
        self.push_document(source, target, "AgentRequest", &request.doc_id)
            .await?;
        if !first_arrival {
            let terminal = crate::support::load_request_row_by_logical_id(
                from.db.node.as_ref(),
                physical_child,
            )
            .await;
            anyhow::ensure!(
                terminal
                    .lifecycle_state
                    .is_some_and(|state| state.is_terminal()),
                "modeled terminal transfer source is not terminal"
            );
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let received = crate::support::load_request_row_by_logical_id(
                    self.node(target)?.db.node.as_ref(),
                    physical_child,
                )
                .await;
                if received.lifecycle_state == terminal.lifecycle_state
                    && received.terminal_output == terminal.terminal_output
                {
                    break;
                }
                anyhow::ensure!(
                    tokio::time::Instant::now() < deadline,
                    "R5 terminal AgentRequest/{physical_child} did not converge on {target}: source {:?}/{:?}, target {:?}/{:?}",
                    terminal.lifecycle_state, terminal.terminal_output,
                    received.lifecycle_state, received.terminal_output
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        Ok(())
    }

    pub fn generated_child_request_id(&self, child: &str) -> Result<&str> {
        self.generated_bridges
            .values()
            .find(|bridge| bridge.symbolic_child == child)
            .with_context(|| format!("R5 child {child} has no accepted bridge"))?
            .physical_child_request_id
            .as_deref()
            .with_context(|| format!("R5 child {child} has no physical reservation"))
    }

    /// Exact notification and wake identities belonging to modeled accepted
    /// bridges. Native setup may create other lawful child rows (notably the
    /// ancestry needed to reach the depth ceiling); those are checked for
    /// physical lineage at creation but are not R5 scenario actions.
    pub fn modeled_completion_keys(&self) -> Result<(HashSet<String>, HashSet<String>)> {
        let mut notifications = HashSet::new();
        let mut wakes = HashSet::new();
        for bridge in self.generated_bridges.values() {
            let child = bridge
                .physical_child_request_id
                .as_deref()
                .with_context(|| {
                    format!(
                        "R5 accepted bridge {} has no physical child",
                        bridge.symbolic_child
                    )
                })?;
            notifications.insert(format!(
                "background-completion-notification:{child}:subagent"
            ));
            wakes.insert(format!("background_completion:{}", bridge.session_id));
        }
        Ok((notifications, wakes))
    }

    async fn publish_generated_child_terminal(
        &self,
        child: &str,
        terminal: &str,
        has_message: bool,
    ) -> Result<()> {
        let physical_child = self.generated_child_request_id(child)?;
        match terminal {
            "completed" => {
                anyhow::ensure!(
                    has_message,
                    "R5 completed child requires its modeled message"
                );
                self.generated_child_backend
                    .as_ref()
                    .context("R5 child provider backend was not prepared")?
                    .release(&format!("R5 child {child}"));
                crate::support::live_inference::wait_for_request_terminal(
                    self.b.db.node.as_ref(),
                    physical_child,
                    std::time::Duration::from_secs(20),
                )
                .await;
            }
            "failed" => {
                anyhow::ensure!(
                    !has_message,
                    "R5 {terminal} child cannot claim a modeled output header"
                );
            }
            "interrupted" => {
                anyhow::ensure!(
                    !has_message,
                    "R5 interrupted child cannot claim a modeled output header"
                );
                crate::support::live_inference::wait_for_request_terminal(
                    self.b.db.node.as_ref(),
                    physical_child,
                    std::time::Duration::from_secs(20),
                )
                .await;
            }
            other => bail!("R5 model exported unknown child terminal {other}"),
        }
        let row =
            crate::support::load_request_row_by_logical_id(self.b.db.node.as_ref(), physical_child)
                .await;
        anyhow::ensure!(
            row.lifecycle_state
                .is_some_and(|state| state.as_str() == terminal),
            "R5 child {child} expected {terminal}, observed {:?}; failure={:?}; lease={:?}",
            row.lifecycle_state,
            row.failure_reason,
            row.execution_lease_expires_at
        );
        anyhow::ensure!(
            matches!(
                (has_message, row.terminal_output.as_ref()),
                (true, Some(TerminalOutput::Message { .. }))
                    | (false, Some(TerminalOutput::NoMessage))
            ),
            "R5 child {child} terminal selection disagrees with generated has_message={has_message}: {:?}",
            row.terminal_output
        );
        Ok(())
    }

    async fn replicate_generated_output_segments(
        &self,
        child: &str,
        source: &NodeId,
        target: &NodeId,
    ) -> Result<()> {
        let physical_child = self.generated_child_request_id(child)?;
        let request = wait_for_child(self.node(source)?.db.node.as_ref(), physical_child).await;
        let row = crate::support::load_request_row_by_logical_id(
            self.node(source)?.db.node.as_ref(),
            physical_child,
        )
        .await;
        let Some(TerminalOutput::Message { message_doc_id }) = row.terminal_output else {
            bail!("R5 child {child} has no terminal assistant header to transfer");
        };
        let header_query = format!(
            "{{ AgentMessage(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
            escape_graphql_string(&message_doc_id),
            gents::session::canonical_rows::AGENT_MESSAGE_FIELDS
        );
        let header_response = self.node(source)?.db.node.execute(&header_query).await;
        if header_response.has_errors() {
            bail!(
                "query R5 terminal header failed: {:?}",
                header_response.errors
            );
        }
        let header_rows = header_response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .context("R5 terminal header query omitted rows")?;
        anyhow::ensure!(
            header_rows.len() == 1,
            "R5 child {child} terminal header is missing or ambiguous"
        );
        let header =
            gents::session::canonical_rows::decode_transcript_message_row(&header_rows[0])?;
        anyhow::ensure!(
            header.message.request_doc_id.as_deref() == Some(request.doc_id.as_str()),
            "R5 child {child} terminal header belongs to another request"
        );
        let close_ids = header
            .message
            .payload_references()
            .into_iter()
            .map(|reference| reference.close_doc_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let query = format!(
            "{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: \"{}\" }} }}) {{ {} }} }}",
            escape_graphql_string(&request.doc_id),
            gents::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS
        );
        let response = self.node(source)?.db.node.execute(&query).await;
        if response.has_errors() {
            bail!("query R5 output segments failed: {:?}", response.errors);
        }
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentOutputSegment"))
            .and_then(serde_json::Value::as_array)
            .context("R5 output segment query omitted rows")?;
        let segments: Vec<gents::session::canonical_rows::OutputSegmentRow> = rows
            .iter()
            .map(gents::session::canonical_rows::decode_output_segment_row)
            .collect::<Result<_>>()?;
        let selected_sources: std::collections::HashSet<OutputSource> = segments
            .iter()
            .filter(|row| close_ids.contains(row.doc_id.as_str()))
            .map(|row| row.segment.source.clone())
            .collect();
        anyhow::ensure!(
            close_ids
                .iter()
                .all(|id| segments.iter().any(|row| row.doc_id.as_str() == *id)),
            "R5 child {child} terminal header references a missing output closure"
        );
        let mut doc_ids: Vec<String> = segments
            .iter()
            .filter(|row| selected_sources.contains(&row.segment.source))
            .map(|row| row.doc_id.clone())
            .collect();
        doc_ids.sort();
        for doc_id in &doc_ids {
            anyhow::ensure!(
                !physical_document_exists(self.node(target)?, "AgentOutputSegment", doc_id).await?,
                "R5 child {child} output arrived before modeled ReplicateOutputSegments"
            );
            self.push_document(source, target, "AgentOutputSegment", doc_id)
                .await?;
        }
        Ok(())
    }

    async fn replicate_generated_message_header(
        &self,
        child: &str,
        source: &NodeId,
        target: &NodeId,
    ) -> Result<()> {
        let physical_child = self.generated_child_request_id(child)?;
        let row = crate::support::load_request_row_by_logical_id(
            self.node(source)?.db.node.as_ref(),
            physical_child,
        )
        .await;
        let Some(TerminalOutput::Message { message_doc_id }) = row.terminal_output else {
            bail!("R5 child {child} has no terminal assistant header to transfer");
        };
        anyhow::ensure!(
            !physical_document_exists(self.node(target)?, "AgentMessage", &message_doc_id).await?,
            "R5 child {child} header arrived before modeled ReplicateMessageHeader"
        );
        self.push_document(source, target, "AgentMessage", &message_doc_id)
            .await
    }

    async fn observe_generated_cancel_mirror(&self, tool: &str) -> Result<()> {
        let bridge = self
            .generated_bridges
            .get(tool)
            .with_context(|| format!("R5 cancel mirror has no bridge {tool}"))?;
        let physical_child = bridge
            .physical_child_request_id
            .as_deref()
            .context("R5 cancel mirror bridge has no reserved child")?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let row = crate::support::load_request_row_by_logical_id(
                self.b.db.node.as_ref(),
                physical_child,
            )
            .await;
            if row.interrupt_requested_at.is_some() {
                return Ok(());
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "B runtime did not mirror R5 cancel intent for {tool}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn observe_generated_child_begin(&mut self, child: &str, generation: u64) -> Result<()> {
        let physical_child = self.generated_child_request_id(child)?.to_owned();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let physical_generation = loop {
            let row = crate::support::load_request_row_by_logical_id(
                self.b.db.node.as_ref(),
                &physical_child,
            )
            .await;
            if row.lifecycle_state == Some(RequestLifecycleState::Processing) {
                let physical_generation = row
                    .execution_generation
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .context("R5 processing child omitted owned generation")?;
                let native_lease_secs =
                    read_generated_child_lease_secs(&self.b, &physical_child).await?;
                anyhow::ensure!(
                    native_lease_secs == i64::try_from(self.child_lease_secs)?,
                    "R5 native claim lease differs from exported child_lease_secs: {native_lease_secs}"
                );
                anyhow::ensure!(
                    row.execution_lease_expires_at.is_some(),
                    "R5 processing child omitted durable expiry"
                );
                break physical_generation.to_owned();
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "R5 modeled BeginChild did not observe physical processing for {child}: {:?}",
                row.lifecycle_state
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        let prior = self
            .modeled_generations
            .entry(child.to_owned())
            .or_default()
            .insert(generation, physical_generation.clone());
        anyhow::ensure!(
            prior.is_none_or(|prior| prior == physical_generation),
            "R5 symbolic generation {generation} changed its physical owner for {child}"
        );
        Ok(())
    }

    async fn wait_for_generated_expired_child_lease(&self, child: &str) -> Result<()> {
        let wait_limit = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let physical_child = self.generated_child_request_id(child)?;
        let row =
            crate::support::load_request_row_by_logical_id(self.b.db.node.as_ref(), physical_child)
                .await;
        anyhow::ensure!(
            row.lifecycle_state == Some(RequestLifecycleState::Processing),
            "R5 AwaitChildExpiry requires a processing child {child}: {:?}",
            row.lifecycle_state
        );
        let expiry = row
            .execution_lease_expires_at
            .as_deref()
            .context("crashed R5 child lost its persisted execution deadline")?;
        let expiry = chrono::DateTime::parse_from_rfc3339(expiry)
            .context("crashed R5 child has malformed execution deadline")?
            .with_timezone(&chrono::Utc);
        while chrono::Utc::now() <= expiry {
            anyhow::ensure!(
                tokio::time::Instant::now() < wait_limit,
                "R5 child execution deadline did not pass within configured test limit"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }

    async fn recover_generated_child_requests(
        &mut self,
        expected_generation: u64,
        fresh_generation: u64,
    ) -> Result<()> {
        anyhow::ensure!(
            expected_generation != fresh_generation,
            "R5 recovery generation symbols must be distinct"
        );
        let mut candidates = Vec::new();
        for (symbolic_child, generations) in &self.modeled_generations {
            let Some(expected_physical) = generations.get(&expected_generation) else {
                continue;
            };
            let physical_child = self.generated_child_request_id(symbolic_child)?.to_owned();
            let row = crate::support::load_request_row_by_logical_id(
                self.b.db.node.as_ref(),
                &physical_child,
            )
            .await;
            if row.lifecycle_state == Some(RequestLifecycleState::Processing) {
                anyhow::ensure!(
                    row.execution_generation.as_deref() == Some(expected_physical.as_str()),
                    "R5 child {symbolic_child} no longer owns modeled generation {expected_generation}"
                );
                candidates.push((
                    symbolic_child.clone(),
                    physical_child,
                    expected_physical.clone(),
                ));
            }
        }
        anyhow::ensure!(
            !candidates.is_empty(),
            "R5 recovery has no processing child bound to modeled generation {expected_generation}"
        );
        let report = RequestLifecycle::recover_all(self.b.db.node.as_ref(), self.b.did()).await?;
        anyhow::ensure!(
            report.requests_recovered >= candidates.len(),
            "R5 real request recovery did not terminalize every modeled expired child: {report:?}"
        );
        for (symbolic_child, physical_child, old_generation) in candidates {
            let row = crate::support::load_request_row_by_logical_id(
                self.b.db.node.as_ref(),
                &physical_child,
            )
            .await;
            anyhow::ensure!(
                row.lifecycle_state
                    .is_some_and(RequestLifecycleState::is_terminal),
                "R5 recovered child {symbolic_child} is not terminal: {:?}",
                row.lifecycle_state
            );
            let next_generation = row
                .execution_generation
                .as_deref()
                .filter(|value| !value.is_empty())
                .context("R5 recovered child omitted fresh physical generation")?;
            anyhow::ensure!(
                next_generation != old_generation,
                "R5 recovery reused the expired physical generation for {symbolic_child}"
            );
            let prior = self
                .modeled_generations
                .get_mut(&symbolic_child)
                .context("R5 child generation binding disappeared")?
                .insert(fresh_generation, next_generation.to_owned());
            anyhow::ensure!(
                prior.is_none_or(|prior| prior == next_generation),
                "R5 fresh generation symbol changed its physical owner for {symbolic_child}"
            );
        }
        Ok(())
    }

    fn record_cancel_ack_outcomes(&mut self, outcomes: Vec<CancelAckOutcome>) {
        self.observed_cancel_ack_events
            .extend(outcomes.into_iter().map(|outcome| {
                let (tool, outcome) = match outcome {
                    CancelAckOutcome::Pending {
                        parent_tool_call_id,
                    } => (parent_tool_call_id, ModeledCancelAckOutcome::Pending),
                    CancelAckOutcome::Stuck {
                        parent_tool_call_id,
                        ..
                    } => (parent_tool_call_id, ModeledCancelAckOutcome::Stuck),
                    CancelAckOutcome::Acked {
                        parent_tool_call_id,
                    } => (parent_tool_call_id, ModeledCancelAckOutcome::Acked),
                };
                ModeledCancelAckEvent { tool, outcome }
            }));
    }

    pub fn observed_cancel_ack_events(&self) -> &[ModeledCancelAckEvent] {
        &self.observed_cancel_ack_events
    }

    fn assert_recovery_checkpoint(&self, checkpoint: &ModeledRecoveryCheckpoint) -> Result<()> {
        let snapshot = self
            .history
            .last()
            .context("R5 recovery checkpoint omitted observation")?;
        let (modeled_notifications, modeled_wakes) = self.modeled_completion_keys()?;
        let expected_notifications = checkpoint
            .notification_children
            .iter()
            .map(|child| {
                Ok(format!(
                    "background-completion-notification:{}:subagent",
                    self.generated_child_request_id(child)?,
                ))
            })
            .collect::<Result<HashSet<_>>>()?;
        let actual_notifications = snapshot
            .subagent_notifications
            .iter()
            .filter(|key| modeled_notifications.contains(*key))
            .cloned()
            .collect::<HashSet<_>>();
        anyhow::ensure!(
            actual_notifications == expected_notifications,
            "R5 recovery action {} notification children disagree: native={actual_notifications:?}, modeled={expected_notifications:?}",
            checkpoint.after_action
        );
        let expected_wakes = checkpoint
            .wake_sessions
            .iter()
            .map(|session| format!("background_completion:{session}"))
            .collect::<HashSet<_>>();
        let actual_wakes = snapshot
            .background_wakeup_keys
            .iter()
            .filter(|key| modeled_wakes.contains(*key))
            .cloned()
            .collect::<HashSet<_>>();
        anyhow::ensure!(
            actual_wakes == expected_wakes,
            "R5 recovery action {} wake sessions disagree: native={actual_wakes:?}, modeled={expected_wakes:?}",
            checkpoint.after_action
        );
        anyhow::ensure!(
            checkpoint.bridges.len() == self.generated_bridges.len(),
            "R5 recovery checkpoint omitted a generated bridge"
        );
        for expected in &checkpoint.bridges {
            let bridge = snapshot
                .a_bridge_rows
                .iter()
                .find(|bridge| bridge.tool_call_id == expected.tool)
                .with_context(|| format!("R5 recovery lost bridge {}", expected.tool))?;
            anyhow::ensure!(
                bridge.lifecycle_state == expected.state
                    && bridge.child_request_id.as_deref()
                        == Some(self.generated_child_request_id(&expected.child)?),
                "R5 recovery action {} bridge {} differs: native={bridge:?}, modeled={expected:?}",
                checkpoint.after_action,
                expected.tool
            );
        }
        for expected in &checkpoint.children {
            let physical_child = self.generated_child_request_id(&expected.child)?;
            let child = snapshot
                .b_child_requests
                .iter()
                .find(|row| row.request_id == physical_child)
                .with_context(|| format!("R5 recovery lost child {}", expected.child))?;
            let observed_terminal = child
                .lifecycle_state
                .is_terminal()
                .then_some(child.lifecycle_state.as_str());
            anyhow::ensure!(
                observed_terminal == expected.terminal.as_deref()
                    && child.interrupt_requested_at.is_some() == expected.interrupt_requested,
                "R5 recovery action {} child {} differs: native={child:?}, modeled={expected:?}",
                checkpoint.after_action,
                expected.child
            );
        }
        Ok(())
    }

    pub async fn run_modeled(&mut self, scenario: &ModeledScenario) -> Result<()> {
        let mut next_checkpoint = 0;
        for (index, action) in scenario.actions.iter().enumerate() {
            let crashed = match action {
                ModeledAction::CrashNode { node, .. } => Some(node.clone()),
                _ => None,
            };
            self.apply_modeled_action(action).await?;
            self.record_observation_after(crashed).await?;
            if let Some(checkpoint) = scenario.recovery_checkpoints.get(next_checkpoint) {
                if checkpoint.after_action == index {
                    self.assert_recovery_checkpoint(checkpoint)?;
                    next_checkpoint += 1;
                }
            }
        }
        anyhow::ensure!(
            next_checkpoint == scenario.recovery_checkpoints.len(),
            "R5 native trace did not consume every generated recovery checkpoint"
        );
        if let Some(agent) = self.generated_child_agent.take() {
            agent.shutdown().await;
        }
        Ok(())
    }

    async fn apply_modeled_action(&mut self, action: &ModeledAction) -> Result<()> {
        match action {
            ModeledAction::PairPrincipals { node, peer } => {
                self.pair_generated_principals(node, peer).await?
            }
            ModeledAction::PublishAcceptedBackgroundBridge {
                tool,
                child,
                session,
                parent_depth,
            } => {
                self.publish_accepted_background_bridge(tool, child, session, *parent_depth, false)
                    .await?
            }
            ModeledAction::RejectSpawnInvocation {
                tool,
                child,
                session,
                parent_depth,
            } => {
                self.publish_accepted_background_bridge(tool, child, session, *parent_depth, true)
                    .await?
            }
            ModeledAction::ReplicateBridge { tool, source, to } => {
                self.replicate_generated_bridge(tool, source, to, true)
                    .await?
            }
            ModeledAction::ReplicateCancelIntent { tool, source, to } => {
                self.replicate_generated_bridge(tool, source, to, false)
                    .await?
            }
            ModeledAction::MaterializeChild { child, tool } => {
                self.materialize_generated_child(child, tool).await?
            }
            ModeledAction::BeginChild { child, generation } => {
                self.observe_generated_child_begin(child, *generation)
                    .await?
            }
            ModeledAction::AwaitChildExpiry { child } => {
                self.wait_for_generated_expired_child_lease(child).await?
            }
            ModeledAction::ReplicateChild { child, source, to } => {
                self.replicate_generated_child(child, source, to, true)
                    .await?
            }
            ModeledAction::ReplicateTerminalRequest { child, source, to } => {
                self.replicate_generated_child(child, source, to, false)
                    .await?
            }
            ModeledAction::PublishChildTerminal {
                child,
                terminal,
                has_message,
            } => {
                self.publish_generated_child_terminal(child, terminal, *has_message)
                    .await?
            }
            ModeledAction::ReplicateOutputSegments { child, source, to } => {
                self.replicate_generated_output_segments(child, source, to)
                    .await?
            }
            ModeledAction::ReplicateMessageHeader { child, source, to } => {
                self.replicate_generated_message_header(child, source, to)
                    .await?
            }
            ModeledAction::ObserveCompletion => {
                self.observe_generated_background_completion().await?
            }
            ModeledAction::CancelBridge { tool } => cancel_parent_on_a(&self.a, tool).await?,
            ModeledAction::MirrorCancel { tool } => {
                self.observe_generated_cancel_mirror(tool).await?
            }
            ModeledAction::ObserveCancelAck => {
                let outcomes =
                    observe_cancel_cascade_ack(self.a.db.node.clone(), self.a.did()).await?;
                self.record_cancel_ack_outcomes(outcomes);
            }
            ModeledAction::RecoverBridges => {
                let _ = ToolCallLifecycle::recover_all(&self.a.db.node, self.a.did()).await?;
            }
            ModeledAction::RecoverChildRequests {
                expected_generation,
                fresh_generation,
            } => {
                self.recover_generated_child_requests(*expected_generation, *fresh_generation)
                    .await?;
            }
            ModeledAction::CrashNode {
                node,
                durable_reopen_premise,
            } => {
                anyhow::ensure!(*durable_reopen_premise, "R5 crash omitted reopen premise");
                if node == "B" {
                    self.wait_for_generated_child_execution_before_b_crash()
                        .await?;
                }
                self.crash_node(node).await?;
            }
            ModeledAction::AdvanceClock { node, seconds } => {
                advance_r5_clock_effects(self.node(node)?, *seconds).await?
            }
            ModeledAction::Converge => {
                self.observe_generated_background_completion().await?;
                let outcomes =
                    observe_cancel_cascade_ack(self.a.db.node.clone(), self.a.did()).await?;
                self.record_cancel_ack_outcomes(outcomes);
            }
        }
        Ok(())
    }

    pub fn observation_history(&self) -> Vec<Observation> {
        self.history.clone()
    }

    async fn observe_generated_background_completion(&self) -> Result<()> {
        let discovered = terminal_child_request_ids(&self.a).await?;
        for bridge in self.generated_bridges.values() {
            let Some(physical_child) = bridge.physical_child_request_id.as_deref() else {
                continue;
            };
            let on_b = crate::support::load_request_row_by_logical_id(
                self.b.db.node.as_ref(),
                physical_child,
            )
            .await;
            if on_b
                .lifecycle_state
                .is_some_and(|state| state.is_terminal())
            {
                anyhow::ensure!(
                    discovered.iter().any(|id| id == physical_child),
                    "R5 generated terminal child {physical_child} was absent from A's exact linked terminal discovery; A children {:?}",
                    load_child_requests(&self.a).await?
                );
            }
        }
        run_background_completion_on_a(&self.a).await
    }

    fn node(&self, id: &NodeId) -> Result<&HarnessNode> {
        if id == &self.a.id {
            Ok(&self.a)
        } else if id == &self.b.id {
            Ok(&self.b)
        } else {
            bail!("unknown node {id}")
        }
    }

    fn node_mut(&mut self, id: &NodeId) -> Result<&mut HarnessNode> {
        if id == &self.a.id {
            Ok(&mut self.a)
        } else if id == &self.b.id {
            Ok(&mut self.b)
        } else {
            bail!("unknown node {id}")
        }
    }

    async fn crash_node(&mut self, id: &NodeId) -> Result<()> {
        // The previous modeled-action snapshot may predate the child reaching
        // owned execution. The caller waits for that readiness before Crash;
        // sample immediately before *any* process teardown so the next
        // observation checks the entire abort + durable reopen boundary.
        self.record_observation().await?;
        if id == "B" {
            if let Some(agent) = self.generated_child_agent.take() {
                agent.crash().await;
            }
        }
        let after_generation = {
            let node = self.node_mut(id)?;
            let before = node.db.process_generation;
            node.db
                .simulate_process_crash()
                .await
                .map_err(|e| anyhow::anyhow!("Crash({id}) failed: {e}"))?;
            if node.db.process_generation != before + 1 {
                bail!(
                    "Crash({id}): process_generation did not advance ({before} -> {})",
                    node.db.process_generation
                );
            }
            node.db.process_generation
        };
        if self.generated_child_backend.is_some() {
            let b_address = wait_for_listen_addr(self.b.db.node.as_ref()).await;
            let a_address = wait_for_listen_addr(self.a.db.node.as_ref()).await;
            self.a
                .db
                .node
                .p2p()
                .context("R5 A lost P2P transport after crash")?
                .connect_peer(&b_address)
                .await
                .context("reconnect R5 peers after crash")?;
            wait_for_connected_peer(self.a.db.node.as_ref()).await;
            wait_for_connected_peer(self.b.db.node.as_ref()).await;
            for (from, address) in [(&self.a, &b_address), (&self.b, &a_address)] {
                from.db
                    .node
                    .p2p()
                    .context("R5 generated peer has no P2P transport after crash")?
                    .add_replicator(
                        vec!["AgentNetwork".to_string()],
                        Some(address),
                        Default::default(),
                        Vec::new(),
                        None,
                    )
                    .await
                    .context("restore exact-document push route after crash")?;
            }
        }
        tracing::info!(
            node = %id,
            process_generation = after_generation,
            "R5 harness process crash/reopen completed"
        );
        Ok(())
    }

    async fn record_observation(&mut self) -> Result<()> {
        self.record_observation_after(None).await
    }

    async fn record_observation_after(&mut self, crashed_node: Option<NodeId>) -> Result<()> {
        self.history.push(Observation {
            a_bridge_rows: load_bridge_rows(&self.a).await?,
            b_bridge_rows: load_bridge_rows(&self.b).await?,
            a_rejected_spawn_invocation_ids: load_rejected_spawn_invocation_ids(&self.a).await?,
            a_child_requests: load_child_requests(&self.a).await?,
            b_child_requests: load_child_requests(&self.b).await?,
            subagent_notifications: load_subagent_notifications(&self.a).await?,
            background_wakeup_keys: load_background_wakeup_keys(&self.a).await?,
            a_process_generation: self.a.db.process_generation,
            b_process_generation: self.b.db.process_generation,
            crashed_node,
        });
        Ok(())
    }
}

async fn write_generated_scripted_route(
    node: &HarnessNode,
    peer_id: &str,
    peer_address: &str,
) -> Result<()> {
    let peer_id = escape_graphql_string(peer_id);
    let peer_address = escape_graphql_string(peer_address);
    let local_did = escape_graphql_string(node.did());
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}", agent_did: "{local_did}",
                    collections: null, replicator_addresses: ["{peer_address}"],
                    template: "agent-config", source: "r5-scripted-transfer",
                    created_at: "{now}", updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{local_did}", collections: null,
                    replicator_addresses: ["{peer_address}"],
                    template: "agent-config", source: "r5-scripted-transfer",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
            upsert_DataPlanePairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                add: {{
                    peer_id: "{peer_id}", agent_did: "{local_did}",
                    collections: null, replicator_addresses: ["{peer_address}"],
                    template: "agent-config", source: "r5-scripted-transfer",
                    created_at: "{now}", updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{local_did}", collections: null,
                    replicator_addresses: ["{peer_address}"],
                    template: "agent-config", source: "r5-scripted-transfer",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    exec(node, &mutation, "write generated scripted P2P route").await
}

async fn read_generated_child_lease_secs(node: &HarnessNode, request_id: &str) -> Result<i64> {
    #[derive(Deserialize)]
    struct LeaseRow {
        execution_lease_secs: Option<i64>,
    }
    let response = node.db.node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ execution_lease_secs }} }}"#,
        escape_graphql_string(request_id),
    )).await;
    anyhow::ensure!(
        !response.has_errors(),
        "R5 child lease query failed: {:?}",
        response.errors
    );
    let rows: Vec<LeaseRow> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|value| value.get("AgentRequest"))
            .context("R5 child lease query omitted AgentRequest")?
            .clone(),
    )?;
    let [row] = rows.as_slice() else {
        bail!("R5 child lease query did not resolve exactly one request");
    };
    row.execution_lease_secs
        .context("R5 processing child omitted execution_lease_secs")
}

/// Configure the exported lease duration through the behavior-owned execution
/// document before any child is claimed. The native claim/recovery owners
/// still choose the physical generation and terminal outcome.
async fn configure_generated_child_execution(
    node: &HarnessNode,
    lease_secs: u64,
    backend_capacity: usize,
) -> Result<()> {
    const BEHAVIOR: &str = "r5-generated-child-behavior";
    const EXECUTION: &str = "r5-generated-child-execution";
    const BACKEND: &str = "r5-generated-child-backend";
    anyhow::ensure!(lease_secs > 0, "R5 child lease must be positive");
    anyhow::ensure!(
        backend_capacity > 0,
        "R5 child backend capacity must be positive"
    );
    let backend_capacity = i64::try_from(backend_capacity)?;
    let deadline_secs = lease_secs
        .checked_add(30)
        .context("R5 child deadline duration overflow")?;
    let agent_did = node.did().to_string();
    let profile_id = format!("{BEHAVIOR}-inference");
    ConfigAccess::transact_local(
        node.db.node.as_ref(),
        None,
        "test.r5_generated_child_lease",
        |txn| {
            let agent_did = agent_did.clone();
            let profile_id = profile_id.clone();
            Box::pin(async move {
                let (_, mut profile) = read_desired_state_record(
                    txn,
                    Collection::InferenceProfile,
                    &agent_did,
                    &profile_id,
                )
                .await?
                .context("R5 child inference profile was not configured")?;
                profile["execution_id"] = EXECUTION.into();
                let (_, mut backend) = read_desired_state_record(
                    txn,
                    Collection::InferenceBackend,
                    &agent_did,
                    BACKEND,
                )
                .await?
                .context("R5 child inference backend was not configured")?;
                backend["max_concurrent"] = backend_capacity.into();
                let execution = json!({
                    "agent_did": agent_did,
                    "execution_id": EXECUTION,
                    "stream_liveness_timeout_secs": lease_secs,
                    "deadline_duration_secs": deadline_secs,
                });
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::InferenceProfile, profile),
                        (Collection::InferenceBackend, backend),
                        (Collection::InferenceExecution, execution),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await
            })
        },
    )
    .await
    .map(|_| ())
}

/// The delegation destination for fixture targets is the paired peer's actual
/// DID, read from the PeerPairingDesired the harness writes as its first
/// actions. Fixtures never invent a destination principal.

async fn ensure_physical_document(
    node: &HarnessNode,
    collection: &str,
    doc_id: &str,
) -> Result<()> {
    anyhow::ensure!(
        physical_document_exists(node, collection, doc_id).await?,
        "source {collection}/{doc_id} is not durably published"
    );
    Ok(())
}

async fn physical_document_exists(
    node: &HarnessNode,
    collection: &str,
    doc_id: &str,
) -> Result<bool> {
    anyhow::ensure!(
        matches!(
            collection,
            "AgentRequest" | "AgentToolCall" | "AgentOutputSegment" | "AgentMessage"
        ),
        "R5 exact-document push does not support {collection}"
    );
    let query = format!(
        "{{ {collection}(filter: {{ _docID: {{ _eq: \"{}\" }} }}, limit: 2) {{ _docID }} }}",
        escape_graphql_string(doc_id)
    );
    let response = node.db.node.execute(&query).await;
    if response.has_errors() {
        bail!("query {collection}/{doc_id} failed: {:?}", response.errors);
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(serde_json::Value::as_array)
        .context("exact-document query omitted collection")?;
    anyhow::ensure!(rows.len() <= 1, "duplicate physical {collection}/{doc_id}");
    Ok(rows.len() == 1)
}

async fn cancel_parent_on_a(node: &HarnessNode, parent_tool_call_id: &str) -> Result<()> {
    let session_id = session_for_tool(node, parent_tool_call_id).await?;
    let mut lifecycle =
        ToolCallLifecycle::load(node.db.node.clone(), &session_id, parent_tool_call_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("bridge {parent_tool_call_id} not found"))?;
    lifecycle
        .cancel_during_run(CancelCause::Interrupted)
        .await?;
    if let Some(dispatch) = lifecycle.bridge_cancel_cascade_dispatch(node.did()).await? {
        if let CascadeDispatch::Local { child, .. } = dispatch {
            gents::interrupt_request_by_doc_id(
                node.db.node.as_ref(),
                child
                    .doc_id
                    .as_deref()
                    .expect("verified physical cascade child"),
                child
                    .agent_did
                    .as_deref()
                    .expect("verified local child principal"),
                child.requester_did.as_deref(),
            )
            .await?;
        }
    }
    Ok(())
}

async fn session_for_tool(node: &HarnessNode, tool_call_id: &str) -> Result<String> {
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{}" }} }}, limit: 2) {{ session_id }}
        }}"#,
        escape_graphql_string(tool_call_id)
    );
    let response = node.db.node.execute(&query).await;
    #[derive(Deserialize)]
    struct Row {
        session_id: String,
    }
    anyhow::ensure!(
        !response.has_errors(),
        "tool session query failed: {:?}",
        response.errors
    );
    let rows: Vec<Row> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .context("tool session query omitted rows")?
            .clone(),
    )?;
    let [row] = rows.as_slice() else {
        bail!(
            "parent tool call {tool_call_id} resolved to {} rows",
            rows.len()
        );
    };
    Ok(row.session_id.clone())
}

async fn run_background_completion_on_a(node: &HarnessNode) -> Result<()> {
    for request_id in terminal_child_request_ids(node).await? {
        let outcome =
            project_background_subagent_completion(node.db.node.clone(), &request_id, node.did())
                .await?;
        anyhow::ensure!(
            matches!(
                outcome,
                gents::background_completion::BackgroundCompletionOutcome::Projected { .. }
                    | gents::background_completion::BackgroundCompletionOutcome::AlreadyProjected
            ),
            "background projection for child {request_id} did not converge: {outcome:?}"
        );
    }
    Ok(())
}

async fn run_cancel_mirror_on_b(node: &HarnessNode, admitted_parent_did: &str) -> Result<()> {
    use gents::agent::p2p_reconcile::PeerAdmissionAuthority;
    use std::sync::Arc;

    // Enrollment is a controlled input to this scenario. The real mirror owns
    // author coherence, admission checks, child scope, deduplication and writes.
    struct ScenarioPeer(String);
    #[async_trait::async_trait]
    impl PeerAdmissionAuthority for ScenarioPeer {
        async fn fresh_member_authorized(&self, member_did: &str) -> Result<bool> {
            Ok(member_did == self.0)
        }
        async fn fresh_member_authorized_for_agent(
            &self,
            member_did: &str,
            _owner_agent: &str,
        ) -> Result<bool> {
            self.fresh_member_authorized(member_did).await
        }
    }

    let snapshot = Arc::new(gents::ActiveRuntimeSnapshot {
        generation: node.db.process_generation,
        principal: None,
        local_did: node.did().to_string(),
        default_behavior_id: String::new(),
        behaviors: Default::default(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_behaviors: Default::default(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: Default::default(),
        behavior_executor_capacities: Default::default(),
        behavior_executor_queue_capacities: Default::default(),
    });
    gents::__test_internals::scan_cross_deployment_cancel_intents(
        node.db.node.clone(),
        snapshot,
        Arc::new(ScenarioPeer(admitted_parent_did.to_string())),
    )
    .await
}

async fn advance_r5_clock_effects(node: &HarnessNode, seconds: u64) -> Result<()> {
    let past = (chrono::Utc::now() - chrono::Duration::seconds(seconds as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let query = r#"{
        AgentToolCall(filter: { cancel_pending_remote_ack: { _eq: true } }) {
            _docID
            started_at
            deadline_at
            completed_at
            unclaimed_deadline_at
            stuck_since
        }
    }"#;
    let response = node.db.node.execute(query).await;
    if response.has_errors() {
        bail!(
            "query cancel-pending bridge rows failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<AdvanceBridgeRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentToolCall"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    for row in rows {
        let doc_id = escape_graphql_string(&row.doc_id);
        let datetime_fields = row.datetime_update_fragment();
        let mutation = format!(
            r#"mutation {{
                update_AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{ cancel_cascade_intent_at: "{past}"{datetime_fields} }}
                ) {{ _docID }}
            }}"#
        );
        exec(node, &mutation, "advance R5 clock effects").await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AdvanceBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    started_at: Option<String>,
    deadline_at: Option<String>,
    completed_at: Option<String>,
    unclaimed_deadline_at: Option<String>,
    stuck_since: Option<String>,
}

impl AdvanceBridgeRow {
    fn datetime_update_fragment(&self) -> String {
        let mut fields = Vec::new();
        push_runner_datetime_field(&mut fields, "started_at", self.started_at.as_deref());
        push_runner_datetime_field(&mut fields, "deadline_at", self.deadline_at.as_deref());
        push_runner_datetime_field(&mut fields, "completed_at", self.completed_at.as_deref());
        push_runner_datetime_field(
            &mut fields,
            "unclaimed_deadline_at",
            self.unclaimed_deadline_at.as_deref(),
        );
        push_runner_datetime_field(&mut fields, "stuck_since", self.stuck_since.as_deref());
        if fields.is_empty() {
            String::new()
        } else {
            format!(", {}", fields.join(", "))
        }
    }
}

fn push_runner_datetime_field(fields: &mut Vec<String>, field: &'static str, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    let value = chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|_| value.to_string());
    fields.push(format!(r#"{field}: "{}""#, escape_graphql_string(&value)));
}

async fn load_bridge_rows(node: &HarnessNode) -> Result<Vec<BridgeObservation>> {
    let query = r#"{
        AgentToolCall(filter: { await_mode: { _eq: "background" } }) {
            request_id
            session_id
            tool_call_id
            lifecycle_state
            child_request_id
            cancel_cause
            cancel_cascade_intent_at
            cancel_pending_remote_ack
            stuck_since
        }
    }"#;
    let response = node.db.node.execute(query).await;
    if response.has_errors() {
        bail!("load bridge rows failed: {:?}", response.errors);
    }
    serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .context("bridge observation query omitted AgentToolCall")?
            .clone(),
    )
    .context("decode canonical R5 bridge observations")
}

async fn load_rejected_spawn_invocation_ids(node: &HarnessNode) -> Result<Vec<String>> {
    let response = node
        .db
        .node
        .execute(
            r#"{
        AgentToolCall(filter: {
            tool_name: { _eq: "spawn_subagent" },
            await_mode: { _eq: "foreground" },
            lifecycle_state: { _eq: "failed" },
            tool_failure_class: { _eq: "argumentInvalid" }
        }) { tool_call_id child_request_id }
    }"#,
        )
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "query rejected spawn invocations failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .context("rejected spawn observation omitted AgentToolCall")?;
    rows.iter()
        .map(|row| {
            anyhow::ensure!(
                row["child_request_id"].is_null(),
                "rejected spawn reserved a child: {row}"
            );
            row["tool_call_id"]
                .as_str()
                .map(str::to_owned)
                .context("rejected spawn lacks native tool identity")
        })
        .collect()
}

async fn load_child_requests(node: &HarnessNode) -> Result<Vec<RequestObservation>> {
    let query = r#"{
        AgentRequest {
            request_id
            agent_did
            lifecycle_state
            caused_by_parent_tool_call_id
            interrupt_requested_at
        }
    }"#;
    let response = node.db.node.execute(query).await;
    if response.has_errors() {
        bail!("load child requests failed: {:?}", response.errors);
    }
    let rows: Vec<RequestObservation> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .context("child observation query omitted AgentRequest")?
            .clone(),
    )
    .context("decode canonical R5 request observations")?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            row.caused_by_parent_tool_call_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
        })
        .collect())
}

async fn terminal_child_request_ids(node: &HarnessNode) -> Result<Vec<String>> {
    Ok(load_child_requests(node)
        .await?
        .into_iter()
        .filter(RequestObservation::is_terminal)
        .map(|row| row.request_id)
        .collect())
}

async fn load_subagent_notifications(node: &HarnessNode) -> Result<Vec<String>> {
    let query = r#"{ AgentMessage { message_key } }"#;
    let response = node.db.node.execute(query).await;
    if response.has_errors() {
        bail!(
            "load canonical notification headers failed: {:?}",
            response.errors
        );
    }
    #[derive(Deserialize)]
    struct Row {
        message_key: String,
    }
    let rows: Vec<Row> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .context("notification query omitted AgentMessage")?
            .clone(),
    )
    .context("decode canonical R5 notification headers")?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            gents::background_completion::is_background_completion_notification_message_key(
                &row.message_key,
            ) && row.message_key.ends_with(":subagent")
        })
        .map(|row| row.message_key)
        .collect())
}

async fn load_background_wakeup_keys(node: &HarnessNode) -> Result<Vec<String>> {
    let query = format!(
        r#"{{
        AgentRequest(filter: {{ agent_did: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{
            request_id input
        }}
    }}"#,
        escape_graphql_string(node.did())
    );
    let response = node.db.node.execute(&query).await;
    if response.has_errors() {
        bail!("load background wakeup keys failed: {:?}", response.errors);
    }
    let rows: Vec<AgentRequestRow> = serde_json::from_value(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .context("background wake query omitted AgentRequest")?
            .clone(),
    )
    .context("decode background wake AgentRequestRow")?;
    let mut keys = Vec::new();
    for row in rows {
        let Some(queue) = row.input.as_ref().and_then(|input| input.queue.as_ref()) else {
            // Ordinary scheduled tasks do not carry queue input.
            continue;
        };
        if queue.source == QueueSource::BackgroundCompletion {
            let key = queue
                .key
                .as_ref()
                .filter(|key| !key.trim().is_empty())
                .with_context(|| {
                    format!(
                        "background wake request {} has no queue key",
                        row.request_id
                    )
                })?;
            keys.push(key.clone());
        }
    }
    Ok(keys)
}

struct HarnessRequest {
    doc_id: String,
    request_id: String,
    session_id: String,
}

impl From<AgentRequestRow> for HarnessRequest {
    fn from(row: AgentRequestRow) -> Self {
        Self {
            doc_id: row.doc_id.expect("AgentRequest._docID"),
            request_id: row.request_id,
            session_id: row.session_id.expect("AgentRequest.session_id"),
        }
    }
}

async fn load_request(node: &HarnessNode, request_id: &str) -> Result<HarnessRequest> {
    load_request_optional(node, request_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("AgentRequest {request_id} not found"))
}

async fn load_request_optional(
    node: &HarnessNode,
    request_id: &str,
) -> Result<Option<HarnessRequest>> {
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                _docID request_id agent_did behavior_id session_id lifecycle_state interrupt_requested_at
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    let response = node.db.node.execute(&query).await;
    Ok(first_optional_row::<AgentRequestRow>(&response, "AgentRequest").map(HarnessRequest::from))
}

async fn exec(node: &HarnessNode, mutation: &str, label: &str) -> Result<()> {
    let response = node.db.node.execute(mutation).await;
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}
