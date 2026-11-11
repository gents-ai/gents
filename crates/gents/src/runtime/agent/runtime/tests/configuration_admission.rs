use super::super::router::configuration_admission::{
    configuration_admission, routed_configuration_admission, ConfigurationAdmission,
    DeferredRequests,
};
use super::support::request;
use crate::documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::inference::resolved::visibility::ConfigurationVisibilityError;
use crate::inference::resolved::{ResolvedConfiguration, SelectedConfiguration};
use crate::runtime::agent::{resolve_document_runtime_snapshot, DocumentResolveContext};
use crate::store::identity::NodeIdentity;
use crate::Collection;
use serde_json::json;

#[test]
fn configuration_admission_and_deferral_match_lean() {
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().runtime_configuration_admission_cases;
    assert_eq!(cases.len(), 4);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let admission = match configuration_admission(case["visible"].as_bool().unwrap()) {
            ConfigurationAdmission::Claim => "claim",
            ConfigurationAdmission::AwaitNewerGeneration => "await_newer_generation",
        };
        assert_eq!(admission, case["expected"].as_str().unwrap(), "{name}");
        let mut deferred = DeferredRequests::default();
        deferred.defer(
            case["deferred_at"].as_u64().unwrap(),
            request(Some("did:key:agent"), name),
        );
        assert_eq!(
            deferred.release(case["active"].as_u64().unwrap()).is_some(),
            case["retries"].as_bool().unwrap(),
            "{name}"
        );
    }
}

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
    let (db, user, access, binding) =
        crate::test_support::signed_test_node("configuration-admission").await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let owner = runtime.did().to_owned();
    let user_did = identity::Did::new(user.did()).unwrap();
    let sibling = crate::store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await
    .unwrap();
    crate::test_support::install_test_agent_binding_with_access(
        &owner,
        "Sibling fixture agent",
        &sibling,
        &access,
    )
    .await;
    let context = DocumentResolveContext::for_tests(
        runtime.clone(),
        crate::tools::surface::ToolCeiling::readonly(),
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

    let mut agent =
        crate::test_support::test_agent_documents(&owner, "Default fixture agent", &binding)
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
            Box::pin(async move { crate::documents::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    crate::inference::backends::registry::set_backend_probe_status_as(
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
        crate::support::fixtures::test_identity("configuration-admission-outsider")
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
