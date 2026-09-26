//! Native replay of the Lean `SpawnClaimFence` traces (#1807).
//!
//! One node holds both the parent's view and the child host's view, so only
//! traces Lean marks `single_node_replayable` run here. Each modeled action
//! drives its real owner: unclaimed expiry the reconciler and then restart
//! recovery (a duplicate settlement), the ordinary deadline restart recovery,
//! materialization the subagent source (or, for a host acting on a stale view
//! of the bridge, the child creation owner it calls), the cancel mirror, the
//! pre-claim gates, and the cancel-ack observer. Replication steps are
//! immediate on one node. `stop` stands in for the child's own execution loop
//! writing its terminal state.

use super::delegated_child_tests::{install_cross_deployment_behavior, receiver_snapshot};
use super::*;

use crate::identity::AgentIdentity;
use crate::lean_vocab_test::{lean_spawn_fence_cases, LeanSpawnFenceCase, LeanSpawnFenceStep};
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::trigger_engine::cross_deployment_cancel_mirror::CrossDeploymentCancelMirror;
use crate::KeyIdentity;
use serde_json::{json, Value};

const BEHAVIOR_ID: &str = "spawn-fence-child";

struct Fixture {
    node: Arc<EmbeddedNode>,
    coordinator: String,
    host: String,
    host_identity: Arc<dyn AgentIdentity>,
    cross: bool,
    parent_id: String,
    parent_doc_id: String,
    bridge_doc_id: String,
    tool_call_id: String,
    child_id: String,
    _keys: tempfile::TempDir,
}

impl Fixture {
    async fn new(case: &LeanSpawnFenceCase) -> Self {
        let keys = tempfile::tempdir().unwrap();
        let cross = match case.route.as_str() {
            "cross_principal" => true,
            "same_principal" => false,
            other => panic!("{}: unknown route {other}", case.name),
        };
        let coordinator_identity =
            KeyIdentity::load_or_create(keys.path().join("coordinator.key"), None).unwrap();
        let coordinator = coordinator_identity.did().to_owned();
        let host_key = if cross { "host.key" } else { "coordinator.key" };
        let host_identity: Arc<dyn AgentIdentity> =
            Arc::new(KeyIdentity::load_or_create(keys.path().join(host_key), None).unwrap());
        let host = host_identity.did().to_owned();
        // The coordinator node reconstructs its own accepted spawn; the host's
        // owners act only through the snapshot identity and delegated input.
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(keys.path().join("node"))
                .with_node_identity_did(&coordinator)
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        install_cross_deployment_behavior(&node, &host, BEHAVIOR_ID).await;

        let parent_id = format!("fence-parent-{}", case.name);
        let tool_call_id = format!("fence-call-{}", case.name);
        let child_id = format!("fence-child-{}", case.name);
        let mut parent = crate::tool_call_lifecycle::admission_fixture::claimed_signed_request(
            &node,
            &parent_id,
            &format!("fence-session-{}", case.name),
            &coordinator_identity,
            None,
        )
        .await;
        let mut bridge =
            crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
                node.clone(),
                &mut parent,
                &coordinator,
                0,
                crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
                &tool_call_id,
                json!({"name": BEHAVIOR_ID, "prompt": "fenced work", "await_mode": "background"}),
                Some(crate::streaming::SpawnAdmissionPlan {
                    tool_call_id: tool_call_id.clone(),
                    child_request_id: child_id.clone(),
                    spawn_target_did: host.clone(),
                    spawn_behavior_id: BEHAVIOR_ID.into(),
                    delegated_workspace: None,
                    await_mode: AwaitMode::Background,
                }),
                AwaitMode::Background,
                CancelPolicy::Cascade,
                true,
            )
            .await
            .unwrap();
        bridge
            .publish_background_receipt("child started")
            .await
            .unwrap();
        let bridge_doc_id = bridge.doc_id().unwrap().to_owned();
        let parent_doc_id = bridge.request_doc_id().unwrap().to_owned();
        // The spawn owner sets the unclaimed deadline only on a cross-principal
        // route (bound by the R5 cross-principal conformance). It is due in
        // the future until a modeled deadline fires.
        if cross {
            set_bridge_datetime(&node, &bridge_doc_id, "unclaimed_deadline_at", FUTURE).await;
        }
        Self {
            node,
            coordinator,
            host,
            host_identity,
            cross,
            parent_id,
            parent_doc_id,
            bridge_doc_id,
            tool_call_id,
            child_id,
            _keys: keys,
        }
    }

    fn snapshot_rx(&self) -> watch::Receiver<Arc<ActiveRuntimeSnapshot>> {
        let (tx, rx) = watch::channel(Arc::new(receiver_snapshot(
            self.host_identity.clone(),
            BEHAVIOR_ID,
        )));
        std::mem::forget(tx);
        rx
    }

    fn peers(&self) -> HashSet<String> {
        HashSet::from([self.coordinator.clone()])
    }

    async fn bridge(&self) -> Value {
        let response = self
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                    lifecycle_state tool_failure_class unclaimed_deadline_at
                    cancel_cascade_intent_at cancel_pending_remote_ack }} }}"#,
                self.bridge_doc_id
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()["AgentToolCall"][0].clone()
    }

    async fn child(&self) -> Option<Value> {
        let response = self
            .node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{
                    _docID lifecycle_state interrupt_requested_at caused_by_parent_tool_call_doc_id }} }}"#,
                self.child_id
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let rows = response.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .clone();
        assert!(rows.len() <= 1);
        rows.into_iter().next()
    }

    async fn recover(&self) {
        crate::tool_call_lifecycle::ToolCallLifecycle::recover_all(&self.node, &self.coordinator)
            .await
            .unwrap();
    }

    async fn create_child_on_stale_view(&self) {
        if self.cross {
            create_subagent_request_with_trusted_parent_request_id_and_workspace(
                &self.node,
                self.child_id.clone(),
                self.parent_id.clone(),
                self.parent_doc_id.clone(),
                self.tool_call_id.clone(),
                self.bridge_doc_id.clone(),
                0,
                self.host.clone(),
                BEHAVIOR_ID.into(),
                "fenced work".into(),
                None,
                self.coordinator.clone(),
                None,
            )
            .await
            .unwrap();
        } else {
            create_subagent_request_with_request_id_and_workspace(
                &self.node,
                self.child_id.clone(),
                self.parent_id.clone(),
                self.parent_doc_id.clone(),
                self.tool_call_id.clone(),
                self.bridge_doc_id.clone(),
                0,
                self.host.clone(),
                BEHAVIOR_ID.into(),
                "local work".into(),
                None,
                None,
            )
            .await
            .unwrap();
        }
    }

    async fn act(
        &self,
        case: &LeanSpawnFenceCase,
        step: &LeanSpawnFenceStep,
        next: Option<&LeanSpawnFenceStep>,
    ) {
        match step.action.as_str() {
            "expire" if !step.enabled => {
                let before = self.bridge().await;
                crate::background_completion::reconcile_unclaimed_cross_deployment_spawns(
                    self.node.clone(),
                    &self.coordinator,
                )
                .await
                .unwrap();
                assert!(!self.cross, "{}", case.name);
                assert!(before["unclaimed_deadline_at"].is_null(), "{}", case.name);
                assert_eq!(self.bridge().await, before, "{}: local expiry", case.name);
            }
            "expire" => {
                set_bridge_datetime(
                    &self.node,
                    &self.bridge_doc_id,
                    "unclaimed_deadline_at",
                    PAST,
                )
                .await;
                crate::background_completion::reconcile_unclaimed_cross_deployment_spawns(
                    self.node.clone(),
                    &self.coordinator,
                )
                .await
                .unwrap();
                self.recover().await;
            }
            "deadline" => {
                set_bridge_datetime(&self.node, &self.bridge_doc_id, "deadline_at", PAST).await;
                if next.is_some_and(|next| next.action == "expire" && next.enabled) {
                    // Both deadlines have passed when restart recovery runs.
                    set_bridge_datetime(
                        &self.node,
                        &self.bridge_doc_id,
                        "unclaimed_deadline_at",
                        PAST,
                    )
                    .await;
                }
                self.recover().await;
            }
            "materialize" if step.stale_host_view => self.create_child_on_stale_view().await,
            "materialize" if !self.cross => {
                // Same-principal authorization through the subagent source is
                // bound by the R5 same-principal conformance; this drives the
                // child creation owner it calls.
                self.create_child_on_stale_view().await
            }
            "materialize" => {
                let mut source = SubagentSource::with_subscription_source_for_test(
                    self.node.clone(),
                    self.snapshot_rx(),
                    self.node.clone(),
                    self.peers(),
                    CancellationToken::new(),
                );
                source
                    .build_intent_for_tool_call_doc(&self.bridge_doc_id)
                    .await
                    .unwrap();
            }
            "publish_child" | "replicate_bridge" => {}
            "mirror" => {
                CrossDeploymentCancelMirror::new(
                    self.node.clone(),
                    self.snapshot_rx(),
                    Arc::new(StaticPeerAdmission {
                        authorized_peer_dids: self.peers(),
                    }),
                    CancellationToken::new(),
                )
                .scan_pending_intents()
                .await
                .unwrap();
            }
            "claim" => {
                let response = self
                    .node
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                        self.child_id,
                        crate::watcher::AGENT_REQUEST_FIELDS
                    ))
                    .await;
                let row: AgentRequestRow = crate::graphql::first_row(&response, "AgentRequest")
                    .unwrap()
                    .expect("claimed child exists");
                let request = crate::watcher::AgentRequest::try_from(row).unwrap();
                let mut lifecycle = RequestLifecycle::new_with_agent_did(
                    self.node.clone(),
                    BEHAVIOR_ID,
                    &self.host,
                    request,
                    60,
                );
                let outcome = lifecycle.claim().await.unwrap();
                assert!(
                    matches!(outcome, ClaimOutcome::Claimed | ClaimOutcome::Interrupted),
                    "{}: {outcome:?}",
                    case.name
                );
            }
            "stop" => {
                let child = self.child().await.expect("stopping child exists");
                let terminal = if child["interrupt_requested_at"].is_null() {
                    "completed"
                } else {
                    "interrupted"
                };
                ConfigAccess::write_local(
                    &self.node,
                    "test.spawn_fence_child_stop",
                    &format!(
                        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }},
                            input: {{ lifecycle_state: "{terminal}" }}) {{ _docID }} }}"#,
                        child["_docID"].as_str().unwrap()
                    ),
                )
                .await
                .unwrap();
            }
            "observe_ack" => {
                crate::background_completion::observe_cancel_cascade_ack(
                    self.node.clone(),
                    &self.coordinator,
                )
                .await
                .unwrap();
            }
            other => panic!("{}: unknown action {other}", case.name),
        }
    }

    async fn assert_matches(
        &self,
        case: &LeanSpawnFenceCase,
        index: usize,
        step: &LeanSpawnFenceStep,
    ) {
        let at = format!("{} step {index} ({})", case.name, step.action);
        let bridge = self.bridge().await;
        let observed_bridge = match bridge["lifecycle_state"].as_str() {
            Some("failed") => {
                assert_eq!(bridge["tool_failure_class"], "spawnUnclaimed", "{at}");
                "abandoned"
            }
            Some("timedOut") if bridge["cancel_cascade_intent_at"].is_string() => "expired",
            Some("timedOut") => "settled_observed",
            Some("running") if self.cross && bridge["unclaimed_deadline_at"].is_null() => "linked",
            Some("running") => "awaiting",
            other => panic!("{at}: bridge lifecycle {other:?}"),
        };
        assert_eq!(observed_bridge, step.bridge, "{at}: bridge");
        assert_eq!(
            !bridge["cancel_cascade_intent_at"].is_null(),
            step.cancel_intent,
            "{at}: cancel intent"
        );
        assert_eq!(
            bridge["cancel_pending_remote_ack"] == true,
            step.ack_pending,
            "{at}: ack pending"
        );
        // One node sees a child row as soon as it exists; the model's separate
        // visibility bit only decides expiry, which the traces order so that
        // the linked/abandoned outcome above already checks it.
        let child = self.child().await;
        assert_eq!(child.is_some(), step.child != "absent", "{at}: child row");
        let observed_child = match child.as_ref() {
            None => "absent",
            Some(child) => {
                // A materialized child always keeps its physical parent binding.
                assert_eq!(
                    child["caused_by_parent_tool_call_doc_id"].as_str(),
                    Some(self.bridge_doc_id.as_str()),
                    "{at}"
                );
                match child["lifecycle_state"].as_str() {
                    Some("pending") => "pending",
                    Some("claimed" | "processing") => "running",
                    Some("interrupted") => "interrupted",
                    Some("completed") => "finished",
                    other => panic!("{at}: child lifecycle {other:?}"),
                }
            }
        };
        assert_eq!(observed_child, step.child, "{at}: child");
        assert_eq!(
            child
                .as_ref()
                .is_some_and(|child| !child["interrupt_requested_at"].is_null()),
            step.interrupt_latched,
            "{at}: interrupt latch"
        );
    }
}

const PAST: &str = "2020-01-01T00:00:00Z";
const FUTURE: &str = "2099-01-01T00:00:00Z";

/// Test clock: moves one bridge deadline so a modeled expiry is due.
async fn set_bridge_datetime(node: &EmbeddedNode, bridge_doc_id: &str, field: &str, value: &str) {
    ConfigAccess::write_local(
        node,
        "test.spawn_fence_deadline",
        &format!(
            r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{bridge_doc_id}" }} }},
                input: {{ {field}: "{value}" }}) {{ _docID }} }}"#
        ),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn generated_spawn_fence_cases_replay_native_owners() {
    let cases = lean_spawn_fence_cases();
    assert_eq!(cases.len(), 10);
    let replayable = cases
        .iter()
        .filter(|case| case.single_node_replayable)
        .collect::<Vec<_>>();
    assert_eq!(replayable.len(), 9);
    for case in replayable {
        let fixture = Fixture::new(case).await;
        assert_eq!(
            !fixture.bridge().await["unclaimed_deadline_at"].is_null(),
            case.unclaimed_deadline_set,
            "{}",
            case.name
        );
        for (index, step) in case.steps.iter().enumerate() {
            fixture.act(case, step, case.steps.get(index + 1)).await;
            fixture.assert_matches(case, index, step).await;
        }
        fixture.node.shutdown().await;
    }
}

#[tokio::test]
async fn unrelated_row_reusing_the_child_id_neither_links_nor_is_interrupted() {
    let case = lean_spawn_fence_cases()
        .iter()
        .find(|case| case.name == "cross_expiry_refuses_materialization")
        .unwrap();
    let fixture = Fixture::new(case).await;
    // Same logical request id, but no lineage to this bridge.
    let unrelated = ConfigAccess::write_local(
        &fixture.node,
        "test.spawn_fence_unrelated_child_id",
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{
                request_id: "{}", agent_did: "{}", behavior_id: "{BEHAVIOR_ID}",
                session_id: "unrelated-session", content: "unrelated",
                lifecycle_state: "pending", created_at: "2026-09-25T14:35:00Z",
                subagent_depth: 0
            }}) {{ _docID }} }}"#,
            fixture.child_id, fixture.host
        ),
    )
    .await
    .unwrap();
    let unrelated_doc_id = crate::graphql::created_doc_id(&unrelated, "AgentRequest").unwrap();

    set_bridge_datetime(
        &fixture.node,
        &fixture.bridge_doc_id,
        "unclaimed_deadline_at",
        PAST,
    )
    .await;
    let outcomes = crate::background_completion::reconcile_unclaimed_cross_deployment_spawns(
        fixture.node.clone(),
        &fixture.coordinator,
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcomes.as_slice(),
            [crate::background_completion::UnclaimedSpawnReconcileOutcome::Failed { .. }]
        ),
        "{outcomes:?}"
    );
    let bridge = fixture.bridge().await;
    assert_eq!(bridge["lifecycle_state"], "failed");
    assert_eq!(bridge["tool_failure_class"], "spawnUnclaimed");
    assert!(bridge["cancel_cascade_intent_at"].is_string());
    assert_eq!(bridge["cancel_pending_remote_ack"], true);

    CrossDeploymentCancelMirror::new(
        fixture.node.clone(),
        fixture.snapshot_rx(),
        Arc::new(StaticPeerAdmission {
            authorized_peer_dids: fixture.peers(),
        }),
        CancellationToken::new(),
    )
    .scan_pending_intents()
    .await
    .unwrap();
    crate::background_completion::observe_cancel_cascade_ack(
        fixture.node.clone(),
        &fixture.coordinator,
    )
    .await
    .unwrap();
    let response = fixture
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{unrelated_doc_id}" }} }}) {{ lifecycle_state interrupt_requested_at }} }}"#
        ))
        .await;
    let row = &response.data.unwrap()["AgentRequest"][0];
    assert_eq!(row["lifecycle_state"], "pending");
    assert!(row["interrupt_requested_at"].is_null());
    assert_eq!(fixture.bridge().await["cancel_pending_remote_ack"], true);
    fixture.node.shutdown().await;
}
