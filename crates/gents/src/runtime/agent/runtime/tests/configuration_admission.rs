use super::super::router::configuration_admission::{
    configuration_admission, routed_configuration_admission, ConfigurationAdmission,
    DeferredRequests,
};
use super::support::request;
use crate::runtime::agent::{resolve_document_runtime_snapshot, DocumentResolveContext};
use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_inference::resolved::visibility::ConfigurationVisibilityError;
use gents_inference::resolved::{ResolvedConfiguration, SelectedConfiguration};
use gents_store::collection::Collection;
use gents_store::identity::NodeIdentity;
use serde_json::json;

fn selection(configuration: &ResolvedConfiguration) -> &SelectedConfiguration {
    match configuration {
        ResolvedConfiguration::Documents(selection) => selection,
        ResolvedConfiguration::Static => panic!("document agents resolve a selection"),
    }
}

/// The client's signed User rebinds the Agent to inference configuration it
/// writes through the desired-state owner, as the desktop component apply
/// does. The generation resolved before that commit no longer holds the
/// Agent's selection, so the router defers instead of claiming; the next
/// generation is readable by the Agent and by no other actor.
#[tokio::test]
async fn client_rebinding_defers_the_superseded_generation_and_scopes_the_new_selection() {
    let (db, user, access, binding) = gents_inference::test_support::signed_test_node_from_db(
        crate::support::trial_store("configuration-admission").await,
        "configuration-admission",
    )
    .await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let owner = runtime.did().to_owned();
    let user_did = identity::Did::new(user.did()).unwrap();
    let sibling = gents_store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await
    .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        &owner,
        "Sibling fixture agent",
        &sibling,
        &access,
    )
    .await;
    let context = DocumentResolveContext::for_tests(
        runtime.clone(),
        gents_tools::surface::ToolCeiling::readonly(),
        Default::default(),
    );
    let first = resolve_document_runtime_snapshot(&node, &context)
        .await
        .unwrap();
    let superseded = first.agents[&binding.agent_did].clone();
    assert_eq!(
        routed_configuration_admission(Some(&superseded.configuration))
            .await
            .unwrap(),
        ConfigurationAdmission::Claim
    );

    let mut agent = gents_documents::test_support::test_agent_documents(
        &owner,
        "Default fixture agent",
        &binding,
    )
    .into_iter()
    .find(|(collection, _)| *collection == Collection::Agent)
    .unwrap()
    .1;
    agent["inference_profile_id"] = json!("editor-setup");
    let plan = DesiredStateApplyPlan::new(
        [
            (
                Collection::InferenceBackend,
                json!({"node_did":owner,"backend_id":"editor-setup","name":"Editor setup","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}}),
            ),
            (
                Collection::InferenceProfile,
                json!({"node_did":owner,"profile_id":"editor-setup","backend_id":"editor-setup","model_name":"editor-fixture"}),
            ),
            (Collection::Agent, agent),
        ]
        .into_iter()
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect(),
    )
    .unwrap();
    access
        .transact_write("test.client_rebinding", |txn| {
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
    let awaiting = resolve_document_runtime_snapshot(&node, &context)
        .await
        .unwrap();
    let unavailable = awaiting.unavailable_agents.get(&binding.agent_did).unwrap();
    assert!(unavailable.awaiting_first_probe);
    assert!(!awaiting.agents.contains_key(&binding.agent_did));
    assert_eq!(
        gents_automation::triggers::runtime_snapshot::effective_agent_admission(
            false,
            Some(unavailable),
            None,
        ),
        gents_automation::triggers::runtime_snapshot::EffectiveAgentAdmission::AwaitFirstProbe,
    );
    gents_inference::backends::registry::set_backend_probe_status_as(
        &access,
        &owner,
        "editor-setup",
        "healthy",
    )
    .await
    .unwrap();

    assert_eq!(
        routed_configuration_admission(Some(&superseded.configuration))
            .await
            .unwrap(),
        ConfigurationAdmission::AwaitNewerGeneration,
        "the superseded generation's selection must not be claimed"
    );
    let second = resolve_document_runtime_snapshot(&node, &context)
        .await
        .unwrap();
    let current = second.agents[&binding.agent_did].clone();
    assert_eq!(current.profile_id.as_deref(), Some("editor-setup"));
    assert_eq!(
        routed_configuration_admission(Some(&current.configuration))
            .await
            .unwrap(),
        ConfigurationAdmission::Claim
    );

    let documents = selection(&current.configuration).documents.clone();
    assert!(documents
        .iter()
        .any(|document| document.collection == Collection::InferenceBackend));
    let sibling_read = SelectedConfiguration::new(
        node.clone(),
        identity::Did::new(&sibling.agent_did).unwrap(),
        documents.clone(),
    )
    .check()
    .await;
    assert!(
        matches!(sibling_read, Err(ConfigurationVisibilityError::Unavailable)),
        "an agent that did not select the configuration must not read it: {sibling_read:?}"
    );
    let outsider = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("configuration-admission-outsider")
    })
    .await
    .unwrap();
    let outsider_read = SelectedConfiguration::new(
        node.clone(),
        identity::Did::new(outsider.did()).unwrap(),
        documents,
    )
    .check()
    .await;
    assert!(
        outsider_read.is_err(),
        "an unrelated actor must not read the selection"
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn configuration_admission_and_deferral_match_lean() {
    let (db, user, access, binding) = gents_inference::test_support::signed_test_node_from_db(
        crate::support::trial_store("configuration-version-admission").await,
        "configuration-version-admission",
    )
    .await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let owner = runtime.did().to_owned();
    let context = DocumentResolveContext::for_tests(
        runtime,
        gents_tools::surface::ToolCeiling::readonly(),
        Default::default(),
    );
    let cases = tokio::task::spawn_blocking(|| {
        gents_lean_vocab::families::contract_cases_runtime_configuration_admission_deferral::cases::<ConfigurationAdmission>()
    }).await.unwrap();
    for case in &cases {
        let input = &case.input;
        let mut deferred = DeferredRequests::default();
        deferred.defer(
            input.deferred_at,
            request(Some("did:key:agent"), input.name.as_str()),
        );
        case.check(
            gents_lean_vocab::families::contract_cases_runtime_configuration_admission_deferral::ConfigurationAdmissionOutput {
                expected: configuration_admission(input.visible, input.versions_current),
                retries: deferred.release(input.active).is_some(),
            },
        );
    }
    let mut previous: Option<std::sync::Arc<gents_inference::resolved::ResolvedAgent>> = None;
    for mode in ["ReadOnly", "Off", "ReadOnly"] {
        let tools = json!({
            "node_did": owner,
            "tools_id": format!("{}:tools", binding.agent_did),
            "host": {"bash": {"mode": mode}}
        });
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::Tools,
            add: tools.clone(),
            update: tools,
        }])
        .unwrap();
        access
            .transact_write("test.in_place_tool_change", |txn| {
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
        if let Some(old) = &previous {
            selection(&old.configuration).check().await.unwrap();
            cases.iter().find(|case| case.input.name == "superseded_same_generation").unwrap().check(
                gents_lean_vocab::families::contract_cases_runtime_configuration_admission_deferral::ConfigurationAdmissionOutput {
                    expected: routed_configuration_admission(Some(&old.configuration)).await.unwrap(),
                    retries: false,
                }
            );
        }
        let snapshot = resolve_document_runtime_snapshot(&node, &context)
            .await
            .unwrap();
        let expected_mode: gents_documents::BashMode = serde_json::from_value(json!(mode)).unwrap();
        assert_eq!(
            snapshot.tool_configs[&binding.agent_did]
                .static_policy()
                .bash
                .tool,
            expected_mode
        );
        let current = snapshot.agents[&binding.agent_did].clone();
        cases.iter().find(|case| case.input.name == "visible").unwrap().check(
            gents_lean_vocab::families::contract_cases_runtime_configuration_admission_deferral::ConfigurationAdmissionOutput {
                expected: routed_configuration_admission(Some(&current.configuration)).await.unwrap(),
                retries: false,
            }
        );
        if let Some(old) = &previous {
            assert_current_router_dispatch(
                old.clone(),
                current.clone(),
                &access,
                &owner,
                user.did(),
            )
            .await;
            assert_ne!(
                gents_inference::connection_fingerprint::agent_slot_fingerprint(old),
                gents_inference::connection_fingerprint::agent_slot_fingerprint(&current),
                "the replacement must advance the resolved generation"
            );
        }
        previous = Some(current);
    }
    drop(previous);
    db.node.shutdown().await.unwrap();
}

async fn assert_current_router_dispatch(
    old: std::sync::Arc<gents_inference::resolved::ResolvedAgent>,
    current: std::sync::Arc<gents_inference::resolved::ResolvedAgent>,
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    user: &str,
) {
    use super::super::router::{RouterInputs, RuntimeAdmissionGate};
    use super::support::{create_user_agent_session, RuntimeStatusHandle};
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};

    struct ObservedWatcher {
        requests: mpsc::Receiver<anyhow::Result<gents_protocol::row::AgentRequest>>,
        waiting: mpsc::UnboundedSender<()>,
    }
    impl gents_session::request::watcher::Watcher for ObservedWatcher {
        async fn next_request(
            &mut self,
        ) -> Option<anyhow::Result<gents_protocol::row::AgentRequest>> {
            self.waiting.send(()).unwrap();
            self.requests.recv().await
        }
    }

    let node = selection(&old.configuration).node.clone();
    let session = uuid::Uuid::new_v4().to_string();
    create_user_agent_session(
        access,
        owner,
        Some(user),
        &session,
        &old.agent_did,
        Some(&old.agent_did),
    )
    .await;
    let (old_tx, mut old_rx) = mpsc::channel(1);
    let old_snapshot = super::router::routed_snapshot_with_agent(1, old.clone(), old_tx);
    let (active_tx, active_rx) = watch::channel(old_snapshot.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (request_tx, request_rx) = mpsc::channel(1);
    let (waiting_tx, mut waiting_rx) = mpsc::unbounded_channel();
    let (status_owner, status) =
        RuntimeStatusHandle::start_with_unbounded_test_clock(node.clone(), owner.to_owned());
    status.initialize_startup(&old.agent_did).await.unwrap();
    status
        .readiness()
        .register_slot(&old.agent_did, 1)
        .await
        .unwrap();
    status
        .readiness()
        .publish_snapshot(old_snapshot.as_ref())
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::runtime::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let gate = RuntimeAdmissionGate::closed();
    gate.open().await;
    let mut routed = request(Some(&old.agent_did), &session);
    routed.node_did = owner.to_owned();
    routed.requester_did = Some(user.to_owned());
    let expected_request = routed.request_id.clone();
    request_tx.send(Ok(routed)).await.unwrap();
    let router_status = status.clone();
    let router_owner = owner.to_owned();
    let router = tokio::spawn(async move {
        super::super::router::run_router_with_watcher(RouterInputs {
            node,
            node_did: router_owner,
            watcher: ObservedWatcher {
                requests: request_rx,
                waiting: waiting_tx,
            },
            active_snapshot_rx: active_rx,
            shutdown: shutdown_rx,
            admission_gate: gate,
            runtime_status: router_status,
            runtime_snapshot_observer: None,
            unresolved_session_bound: 1,
        })
        .await
    });
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(10), waiting_rx.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        old_rx.try_recv().is_err(),
        "superseded settings must not receive the request"
    );
    let (current_tx, mut current_rx) = mpsc::channel(1);
    let snapshot = super::router::routed_snapshot_with_agent(2, current, current_tx);
    status
        .readiness()
        .publish_snapshot(snapshot.as_ref())
        .await
        .unwrap();
    active_tx.send(snapshot).unwrap();
    let dispatched = tokio::time::timeout(Duration::from_secs(10), current_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dispatched.request_id, expected_request);
    assert!(old_rx.try_recv().is_err());
    assert!(current_rx.try_recv().is_err());
    shutdown_tx.send(true).unwrap();
    router.await.unwrap().unwrap();
    status_owner.close().await.unwrap();
}
