use gents_documents::views::agents::AgentList;
use gents_inference::test_support::signed_test_node_without_agent;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};
use gents_store::views::ViewError;
use serde_json::json;

#[tokio::test]
async fn agent_inventory_matches_lean_on_served_node() -> anyhow::Result<()> {
    let (db, _, owner) = signed_test_node_without_agent(
        "agent-inventory-views",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let reader = gents_store::test_support::test_identity("agent-inventory-reader");
    let reader_did = identity::Did::new(reader.did())?;
    let admin =
        ConfigAccess::local_as(db.node.clone(), identity::Did::new(db.node_identity.did())?);
    GrantSet::AgentBaseline.grant(&admin, &reader_did).await?;
    let cases = gents_lean_vocab::families::agent_inventory::cases();
    assert!(!cases.is_empty());
    for document in &cases[0].input.documents {
        let variables = json!({"input": {
            "node_did": document.node_did,
            "agent_did": document.agent_did,
            "creation_nonce": "view-fixture",
            "inference_profile_id": "profile"
        }});
        let response = owner.transact_write("test.agent_view.create", |txn| {
            let variables = &variables;
            Box::pin(async move {
                txn.execute_client_signed_create(
                    "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                    variables,
                    gents_protocol::schemas::AGENT,
                ).await
            })
        }).await?;
        let doc_id = gents_store::graphql::created_doc_id(&response, "Agent")?;
        if document.readable {
            owner
                .add_actor_relationship("Agent", &doc_id, "reader", &reader_did)
                .await?;
        }
    }
    for case in cases {
        let request = &case.input.request;
        let access = if request.reader.is_some() {
            db.client_access(&reader)
        } else {
            ConfigAccess::graphql(db.client_endpoint())
        };
        let mut view = AgentList::new(&request.node_did);
        if let Some(id) = &request.agent_did {
            view = view.agent_did(id);
        }
        if let Some(after) = &request.after {
            view = view.after(after);
        }
        if let Some(limit) = request.limit {
            view = view.limit(limit);
        }
        let observed = match access.read_view(&view).await {
            Ok(rows) => rows.into_iter().map(|row| row.agent.agent_did).collect(),
            Err(ViewError::ActorRequired) if request.reader.is_none() => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        case.check(observed);
    }
    Ok(())
}
