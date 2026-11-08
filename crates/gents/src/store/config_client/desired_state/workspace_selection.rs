use super::{plan::document_identity, read, ConfigApplyTxn, DesiredStateApplyPlan};
use crate::documents::references_of;
use crate::Collection;
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Capture(BTreeMap<(String, String), BTreeSet<(String, String)>>);

fn selected_agents(owner: &str, callback: &Value) -> Result<BTreeSet<String>> {
    Ok(references_of(Collection::Callback, callback, owner)?
        .into_iter()
        .filter(|reference| reference.collection == Collection::Agent)
        .map(|reference| reference.id)
        .collect())
}

async fn links(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    id: &str,
    require_targets: bool,
) -> Result<BTreeSet<(String, String)>> {
    let Some((callback_doc, value)) =
        read::read_record(txn, Collection::Callback, owner, id).await?
    else {
        return Ok(BTreeSet::new());
    };
    let callback: crate::documents::Callback = serde_json::from_value(value.clone())?;
    if !callback.enabled {
        return Ok(BTreeSet::new());
    }
    let mut links = BTreeSet::new();
    for did in selected_agents(owner, &value)? {
        identity::Did::new(&did)?;
        anyhow::ensure!(
            did != owner,
            "workspace selection cannot name the routing Node as an Agent"
        );
        match read::read_record(txn, Collection::Agent, owner, &did).await? {
            Some((doc, value)) => {
                anyhow::ensure!(
                    value["agent_did"] == did && value["node_did"] == owner,
                    "workspace selected Agent changed scope"
                );
                links.insert((doc, callback_doc.clone()));
            }
            None if !require_targets => anyhow::bail!(
                "existing workspace selection is unavailable; cannot safely reconcile its grants"
            ),
            None => anyhow::bail!("configured workspace Agent is unavailable"),
        }
    }
    Ok(links)
}

pub(super) async fn capture(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<Capture> {
    let mut before = BTreeMap::new();
    for document in plan
        .documents()
        .iter()
        .filter(|document| document.collection == Collection::Callback)
    {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        before.insert(
            (owner.to_owned(), id.to_owned()),
            links(txn, owner, id, false).await?,
        );
    }
    Ok(Capture(before))
}

/// Entity-set edges are keyed by physical Callback, so two callbacks sharing an
/// Agent do not erase each other's grants. Unchanged selections do not regrant.
pub(super) async fn apply(txn: &ConfigApplyTxn<'_>, before: Capture) -> Result<()> {
    for ((owner, id), old) in before.0 {
        let new = links(txn, &owner, &id, true).await?;
        for (agent, callback) in old.difference(&new) {
            txn.delete_entity_set_relationship(
                "Agent",
                agent,
                "workspace_selected_by",
                "configuration",
                callback,
                "reader",
            )
            .await?;
        }
        for (agent, callback) in new.difference(&old) {
            txn.add_entity_set_relationship(
                "Agent",
                agent,
                "workspace_selected_by",
                "configuration",
                callback,
                "reader",
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::config_client::{ConfigAccess, DesiredStateApplyDocument, GraphqlEndpoint};
    use crate::store::identity::NodeIdentity;
    use serde_json::json;
    use std::sync::Arc;

    fn client(url: &str, key: &dyn NodeIdentity) -> Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(url, key.did())
                .with_client_signing_capability(key.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        ))
    }

    fn agent_visible(response: serde_json::Value) -> Result<bool> {
        let response: query::QueryResponse = serde_json::from_value(response)?;
        Ok(
            !crate::store::graphql::required_rows::<serde_json::Value>(&response, "Agent")?
                .is_empty(),
        )
    }

    #[test]
    fn workspace_selection_edges_are_the_selected_agents() -> Result<()> {
        use crate::store::config_client::desired_state::selection::tests::canonical;
        let owner = "did:key:node";
        let selection = json!({"writer_agent_did":"did:key:writer",
            "integrator_agent_did":"did:key:integrator",
            "readers":[{"agent_did":"did:key:reader-a"},{"agent_did":"did:key:reader-b"}]});
        let expected = [
            "did:key:integrator",
            "did:key:reader-a",
            "did:key:reader-b",
            "did:key:writer",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
        for handler in [
            json!({"kind":"built_in","emitter":"create_workspace","workspace_agents":selection}),
            json!({"kind":"module","module_id":"module","workspace_agents":selection}),
        ] {
            let callback = canonical(Collection::Callback, owner, json!({"handler": handler}));
            assert_eq!(selected_agents(owner, &callback)?, expected);
        }
        let plugin = canonical(
            Collection::Callback,
            owner,
            json!({"handler":{"kind":"plugin","plugin":"plugin","digest":"digest"}}),
        );
        assert!(selected_agents(owner, &plugin)?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn selected_callback_agents_follow_native_visibility() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let user = crate::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
        let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
        let other = crate::KeyIdentity::load_or_create(directory.path().join("other"), None)?;
        let creator_binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let target_binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let creator = crate::store::identity::resolve_agent_identity(&runtime, &creator_binding)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            defra_node::Node::builder()
                .enable_signing()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(user.did().to_owned())
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let context =
            defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
        for key in [creator.as_ref(), &other as &dyn NodeIdentity] {
            for permission in [
                acp::nac::NodePermission::DocumentRead,
                acp::nac::NodePermission::DocumentUpdate,
                acp::nac::NodePermission::CollectionGet,
            ] {
                node.add_nac_actor_relationship_as(&context, permission.as_str(), key.did())
                    .await?;
            }
        }
        let url = format!("http://{address}/api/v0/graphql");
        let access = client(&url, &user)?;
        let node_response = access.transact_write("test.workspace_selection.node", |txn| Box::pin(async {
            txn.execute_client_signed_create("mutation($input: NodeMutationInputArg!) { create_Node(input:$input) {_docID} }",
                &json!({"input":{"node_did":runtime.did()}}), gents_protocol::schemas::NODE).await
        })).await?;
        let node_doc = crate::store::graphql::created_doc_id(&node_response, "Node")?;
        let target_response = access.transact_write("test.workspace_selection.target", |txn| Box::pin(async {
            let response = txn.execute_client_signed_create("mutation($input: AgentMutationInputArg!) { create_Agent(input:$input) {_docID} }",
                &json!({"input":{"node_did":runtime.did(),"agent_did":target_binding.agent_did,"creation_nonce":target_binding.creation_nonce,"inference_profile_id":format!("{}:inference",target_binding.agent_did),"enabled":true}}), gents_protocol::schemas::AGENT).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "Agent")?;
            txn.add_resource_relationship("Agent", &doc, "parent", "nodes", &node_doc).await?;
            Ok::<_, anyhow::Error>(response)
        })).await?;
        let target_doc = crate::store::graphql::created_doc_id(&target_response, "Agent")?;
        let callback = json!({"node_did":runtime.did(),"callback_id":"workspace-callback",
            "handler":{"kind":"built_in","emitter":"create_workspace","workspace_agents":{
                "writer_agent_did":target_binding.agent_did,"integrator_agent_did":target_binding.agent_did}},
            "capabilities":["create_workspace","observe_dirty_base"]});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::Callback,
            add: callback.clone(),
            update: callback.clone(),
        }])?;
        access
            .transact_write("test.workspace_selection.callback", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::documents::apply_desired_state_plan(txn, plan).await
                })
            })
            .await?;
        let rows = access.execute("{Callback {_docID callback_id}}").await?;
        let callback_doc = rows["data"]["Callback"][0]["_docID"]
            .as_str()
            .expect("callback ID")
            .to_owned();
        access
            .add_actor_relationship(
                "Callback",
                &callback_doc,
                "reader",
                &identity::Did::new(creator.did())?,
            )
            .await?;
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().workspace_callback_selection_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let key: &dyn NodeIdentity = match case["actor"].as_str().unwrap() {
                "user" => &user,
                "creator" => creator.as_ref(),
                "other" => &other,
                actor => panic!("unknown actor {actor}"),
            };
            let response = client(&url, key)?
                .execute(&format!(
                    "{{Agent(filter:{{_docID:{{_eq:\"{}\"}}}}) {{_docID}}}}",
                    crate::store::graphql::escape_graphql_string(&target_doc)
                ))
                .await?;
            let visible = agent_visible(response)?;
            assert_eq!(visible, case["before"].as_bool().unwrap(), "{case}");
        }
        access
            .transact_write("test.workspace_selection.revoke", |txn| {
                let (target_doc, callback_doc) = (&target_doc, &callback_doc);
                Box::pin(async move {
                    txn.delete_entity_set_relationship(
                        "Agent",
                        target_doc,
                        "workspace_selected_by",
                        "configuration",
                        callback_doc,
                        "reader",
                    )
                    .await
                })
            })
            .await?;
        access
            .transact_write("test.workspace_selection.repeat", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::documents::apply_desired_state_plan(txn, plan).await
                })
            })
            .await?;
        for case in cases {
            let key: &dyn NodeIdentity = match case["actor"].as_str().unwrap() {
                "user" => &user,
                "creator" => creator.as_ref(),
                "other" => &other,
                actor => panic!("unknown actor {actor}"),
            };
            let response = client(&url, key)?
                .execute(&format!(
                    "{{Agent(filter:{{_docID:{{_eq:\"{}\"}}}}) {{_docID}}}}",
                    crate::store::graphql::escape_graphql_string(&target_doc)
                ))
                .await?;
            let visible = agent_visible(response)?;
            assert_eq!(
                visible,
                case["after_revocation"].as_bool().unwrap(),
                "{case}"
            );
        }
        for (enabled, expected_key) in [(false, "after_disabled"), (true, "after_reenabled")] {
            let mut update = callback.clone();
            update["enabled"] = json!(enabled);
            let changed = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                collection: Collection::Callback,
                add: update.clone(),
                update,
            }])?;
            access
                .transact_write("test.workspace_selection.enabled", |txn| {
                    let changed = &changed;
                    Box::pin(async move {
                        crate::documents::apply_desired_state_plan(txn, changed).await
                    })
                })
                .await?;
            for case in cases {
                let key: &dyn NodeIdentity = match case["actor"].as_str().unwrap() {
                    "user" => &user,
                    "creator" => creator.as_ref(),
                    "other" => &other,
                    actor => panic!("unknown actor {actor}"),
                };
                let response = client(&url, key)?
                    .execute(&format!(
                        "{{Agent(filter:{{_docID:{{_eq:\"{}\"}}}}) {{_docID}}}}",
                        crate::store::graphql::escape_graphql_string(&target_doc)
                    ))
                    .await?;
                let visible = agent_visible(response)?;
                assert_eq!(visible, case[expected_key].as_bool().unwrap(), "{case}");
            }
        }
        node.shutdown().await?;
        Ok(())
    }
}
