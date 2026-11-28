use std::collections::BTreeSet;

use gents_documents::test_support::desired_state_fixtures::{backend, config, document};
use gents_documents::{apply_desired_state_plan, DesiredStateApplyPlan};
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_store::collection::Collection;
use gents_store::config_client::body_verification::BodyVerification;
use gents_store::config_client::ConfigAccess;
use gents_store::local::document_acp::DocumentAcp;
use serde_json::json;

use crate::test_support::vera_test_home::{Household, PROVIDERS};

async fn apply(
    household: &Household,
    documents: Vec<gents_documents::DesiredStateApplyDocument>,
) -> anyhow::Result<()> {
    let plan = DesiredStateApplyPlan::new(documents)?;
    household
        .as_user()?
        .transact_write("test.body.apply", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
                .map(|_| ())
            })
        })
        .await?;
    household
        .settled(&household.user_did, "the applied configuration")
        .await
}

fn profile(node: &str) -> Vec<gents_documents::DesiredStateApplyDocument> {
    vec![
        document(backend(node, "backend")),
        config(
            Collection::InferenceProfile,
            json!({"node_did": node, "profile_id": "fixture", "backend_id": "backend", "model_name": "model"}),
        ),
    ]
}

/// Configure `agent` as the init User's client does: its backend, profile,
/// tools, context and Agent document in one apply.
async fn configure(
    household: &Household,
    agent: &AgentIdentityBinding,
    display_name: &str,
) -> anyhow::Result<()> {
    let node = household.node_did.as_str();
    let mut documents = profile(node);
    documents.extend([
        config(
            Collection::Tools,
            json!({"node_did": node, "tools_id": "body-tools"}),
        ),
        config(
            Collection::AgentContext,
            json!({"node_did": node, "context_id": "body-context", "tools_id": "body-tools"}),
        ),
        config(
            Collection::Agent,
            json!({"node_did": node, "agent_did": agent.agent_did,
                "creation_nonce": agent.creation_nonce, "display_name": display_name,
                "context_id": "body-context", "inference_profile_id": "fixture", "enabled": true}),
        ),
    ]);
    apply(household, documents).await
}

/// What the runtime's view reads of `agent` in one transaction as the Node:
/// its body and the head of every document `configure` selects for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Read {
    body: BodyVerification,
    agent_doc: String,
    agent_head: Option<String>,
    heads: BTreeSet<String>,
    display_name: Option<String>,
}

async fn read(household: &Household, agent: &AgentIdentityBinding) -> anyhow::Result<Read> {
    let node = household.node_did.clone();
    ConfigAccess::transact_local_readonly(
        &household.node,
        Some(node.clone()),
        "test.body.read",
        |txn| {
            let (node, agent_did) = (node.clone(), agent.agent_did.clone());
            Box::pin(async move {
                let mut heads = BTreeSet::new();
                let mut agent_head = None;
                let mut agent_doc = String::new();
                let mut display_name = None;
                for (name, key, id) in [
                    ("Agent", "agent_did", agent_did.as_str()),
                    ("AgentContext", "context_id", "body-context"),
                    ("Tools", "tools_id", "body-tools"),
                    ("InferenceProfile", "profile_id", "fixture"),
                    ("InferenceBackendMetadata", "backend_id", "backend"),
                ] {
                    let extra = if name == "Agent" { "display_name" } else { "" };
                    let response = txn
                        .execute_with_variables(
                            &format!(
                                "query($id: String) {{ {name}(filter: {{ {key}: {{ _eq: $id }} }}) {{ _docID {extra} _version {{ cid height fieldName }} }} }}"
                            ),
                            &json!({ "id": id }),
                        )
                        .await?;
                    let Some(row) = response["data"][name].get(0) else {
                        continue;
                    };
                    let head = gents_store::graphql::document_composite_version(row, "test.body")?
                        .map(|version| version.cid);
                    if name == "Agent" {
                        agent_doc = row["_docID"].as_str().unwrap_or_default().to_owned();
                        display_name = row["display_name"].as_str().map(str::to_owned);
                        agent_head.clone_from(&head);
                    }
                    heads.extend(head);
                }
                let bodies = gents_documents::placement_bodies(
                    txn,
                    node.as_str(),
                    &[(agent_did, agent_doc.clone())],
                )
                .await?;
                Ok::<_, anyhow::Error>(Read {
                    body: bodies[&agent_doc].clone(),
                    agent_doc,
                    agent_head,
                    heads,
                    display_name,
                })
            })
        },
    )
    .await
}

fn verified_heads(body: &BodyVerification) -> BTreeSet<String> {
    match body {
        BodyVerification::Verified { heads } => heads.iter().cloned().collect(),
        other => panic!("the body is not verified: {other:?}"),
    }
}

/// A node deciding access in its own store merges replicated changes
/// unchecked, so no body is verified there; on a Vera home the body binds the
/// heads read in the same transaction, the Agent's first.
#[tokio::test]
async fn a_body_is_verified_only_on_a_home_that_holds_replicated_changes() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        let household = Household::open(provider).await?;
        let agent = household.derive_agent()?;
        configure(&household, &agent, "first").await?;
        let seen = read(&household, &agent).await?;
        household.shutdown().await?;
        match provider {
            DocumentAcp::Local => assert_eq!(seen.body, BodyVerification::NotStrict),
            DocumentAcp::Vera => {
                assert_eq!(seen.heads.len(), 5, "{seen:?}");
                assert_eq!(verified_heads(&seen.body), seen.heads);
                let BodyVerification::Verified { heads } = &seen.body else {
                    unreachable!()
                };
                assert_eq!(heads.first(), seen.agent_head.as_ref());
            }
        }
    }
    Ok(())
}

async fn rename(access: &ConfigAccess, agent_doc: &str, name: &str) -> anyhow::Result<bool> {
    let updated = access
        .write_with_variables(
            "test.body.rename",
            "mutation($id: ID!, $name: String) { update_Agent(docID: $id, input: {display_name: $name}) { _docID } }",
            &json!({"id": agent_doc, "name": name}),
        )
        .await?;
    Ok(updated["data"]["update_Agent"]
        .as_array()
        .is_some_and(|rows| rows.len() == 1))
}

/// An edit by an actor without `update` commits nothing, so the body stays at
/// the heads last authorized; an edit by the owner moves the Agent's head at
/// once, with no change to its placement.
#[tokio::test]
async fn a_refused_edit_keeps_the_verified_heads_and_an_allowed_edit_moves_them(
) -> anyhow::Result<()> {
    let household = Household::open(DocumentAcp::Vera).await?;
    let agent = household.derive_agent()?;
    configure(&household, &agent, "first").await?;
    let stranger = household.derive_agent()?;
    household.create_agent(&stranger).await?;
    let before = read(&household, &agent).await?;
    let agent_doc = before.agent_doc.clone();
    let stranger_holds_update = household
        .holds(
            &identity::Did::new(&stranger.agent_did)?,
            "update",
            "agents",
            &agent_doc,
        )
        .await?;

    let refused = rename(&household.agent_access(&stranger)?, &agent_doc, "taken").await;
    let after_refusal = read(&household, &agent).await?;

    configure(&household, &agent, "second").await?;
    let after_edit = read(&household, &agent).await?;
    household.shutdown().await?;

    assert!(!stranger_holds_update);
    assert!(!matches!(refused, Ok(true)), "{refused:?}");
    assert_eq!(after_refusal, before);
    assert_eq!(before.display_name.as_deref(), Some("first"));
    assert_eq!(after_edit.display_name.as_deref(), Some("second"));
    assert_eq!(verified_heads(&after_edit.body), after_edit.heads);
    assert_ne!(after_edit.agent_head, before.agent_head);
    assert_ne!(after_edit.body, before.body);
    Ok(())
}

/// A document the configuration selects that has not arrived leaves the body
/// waiting, never refused, and the body is verified once it is there.
#[tokio::test]
async fn a_body_waits_for_a_selected_document_that_is_absent() -> anyhow::Result<()> {
    let household = Household::open(DocumentAcp::Vera).await?;
    let agent = household.derive_agent()?;
    household.create_agent(&agent).await?;
    let waiting = read(&household, &agent).await?;
    apply(&household, profile(household.node_did.as_str())).await?;
    let arrived = read(&household, &agent).await?;
    household.shutdown().await?;

    assert_eq!(waiting.body, BodyVerification::Waiting);
    assert!(arrived.body.is_verified(), "{arrived:?}");
    Ok(())
}

/// The registration read answers for one document under its collection's
/// resource and the home's policy; a document that is not registered there
/// leaves a body waiting (`Placement.body`).
#[tokio::test]
async fn registration_is_read_per_document_under_the_home_policy() -> anyhow::Result<()> {
    let household = Household::open(DocumentAcp::Vera).await?;
    let agent = household.derive_agent()?;
    let agent_doc = household.create_agent(&agent).await?;
    let seen = ConfigAccess::transact_local_readonly(
        &household.node,
        Some(household.node_did.clone()),
        "test.body.registration",
        |txn| {
            let agent_doc = agent_doc.clone();
            Box::pin(async move {
                let home = txn
                    .home_policy()
                    .expect("an embedded transaction has a home");
                Ok::<_, anyhow::Error>((
                    txn.document_registered(home, Collection::Agent, &agent_doc)
                        .await?,
                    txn.document_registered(home, Collection::Tools, &agent_doc)
                        .await?,
                    txn.document_registered("another-home", Collection::Agent, &agent_doc)
                        .await?,
                ))
            })
        },
    )
    .await?;
    household.shutdown().await?;
    assert_eq!(seen, (true, false, false));
    Ok(())
}
