use acp::{DocumentACP, Subject, ZanzibarStore};
use db::DocumentMutationView;
use document::DocID;
use identity::Did;
use storage::corekv::{Error, Result};
use storage::namespace::Namespace;

use gents_protocol::schemas::{
    identity_policy_resource, is_replicated_protocol_collection, AGENT_OWNERSHIP_POLICY_ID,
    NODE_NAME, PEER_REGISTRY_NAME,
};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Other(error.to_string())
}

fn transport_resource(collection: &str) -> Option<&'static str> {
    if is_replicated_protocol_collection(collection) {
        identity_policy_resource(collection)
    } else {
        None
    }
}

async fn local_node(
    view: &DocumentMutationView,
    provider: &dyn DocumentACP,
    runtime: &Did,
) -> Result<Option<(String, Did)>> {
    let system = view.sibling(Namespace::Systemstore);
    let Some(collection) =
        db::collection::loader::load_collection_from_systemstore(&system, NODE_NAME)
            .await
            .map_err(failure)?
    else {
        return Ok(None);
    };
    let policy = collection
        .schema()
        .policy
        .as_ref()
        .ok_or_else(|| failure("local Node has no policy"))?;
    if collection.schema().is_branchable
        || policy.id != AGENT_OWNERSHIP_POLICY_ID
        || policy.resource_name != "nodes"
    {
        return Err(failure("local Node policy is incompatible"));
    }
    let docs = collection
        .get_all_with_datastore_validated(view, &system)
        .await
        .map_err(failure)?;
    let matches: Vec<_> = docs
        .iter()
        .filter(|doc| {
            doc.get("node_did").and_then(|value| value.as_str()) == Some(runtime.as_str())
        })
        .collect();
    let [node] = matches.as_slice() else {
        if matches.is_empty() {
            return Ok(None);
        }
        return Err(failure("local Node identity is ambiguous"));
    };
    let doc = node
        .id()
        .ok_or_else(|| failure("local Node has no document ID"))?
        .to_string();
    let scoped = provider
        .bind_transaction(view.sibling(Namespace::Acpstore))
        .ok_or_else(|| failure("transaction-bound document ACP is unavailable"))?;
    let owner = scoped
        .get_doc_owner(AGENT_OWNERSHIP_POLICY_ID, "nodes", &doc)
        .await
        .map_err(failure)?
        .ok_or_else(|| failure("local Node has no registered owner"))?;
    Ok(Some((doc, owner)))
}

async fn change(
    view: &DocumentMutationView,
    provider: &dyn DocumentACP,
    runtime: &Did,
    resource: &str,
    doc: &str,
    relation: &str,
    target: Subject,
    delete: bool,
) -> Result<()> {
    provider
        .change_node_admin_relationship(
            view.sibling(Namespace::Acpstore),
            runtime,
            target,
            AGENT_OWNERSHIP_POLICY_ID,
            resource,
            doc,
            relation,
            delete,
        )
        .await
        .map_err(failure)?;
    Ok(())
}

/// Stage the receiving runtime's document-scoped native grants in the accepted
/// row's transaction. A completed marker never restores a revoked grant.
pub(crate) async fn project_transport_document(
    view: &DocumentMutationView,
    provider: &dyn DocumentACP,
    runtime: &Did,
    collection_name: &str,
    doc_id: &str,
) -> Result<()> {
    let Some(resource) = transport_resource(collection_name) else {
        return Ok(());
    };
    if doc_id.is_empty() {
        return Ok(());
    }
    let system = view.sibling(Namespace::Systemstore);
    let Some(collection) =
        db::collection::loader::load_collection_from_systemstore(&system, collection_name)
            .await
            .map_err(failure)?
    else {
        return Ok(());
    };
    let schema = collection.schema();
    if !schema.is_branchable
        || !schema.policy.as_ref().is_some_and(|policy| {
            policy.id == AGENT_OWNERSHIP_POLICY_ID && policy.resource_name == resource
        })
    {
        return Err(failure(
            "transport collection has an incompatible native policy",
        ));
    }
    let id = DocID::from_string(doc_id).map_err(failure)?;
    let Some(document) = collection
        .get_by_doc_id(view, &system, &id)
        .await
        .map_err(failure)?
    else {
        return Ok(());
    };
    let acp_view = view.sibling(Namespace::Acpstore);
    let scoped = provider
        .bind_transaction(acp_view.clone())
        .ok_or_else(|| failure("transaction-bound document ACP is unavailable"))?;
    if !scoped
        .is_doc_registered(AGENT_OWNERSHIP_POLICY_ID, resource, doc_id)
        .await
        .map_err(failure)?
    {
        return Err(failure("transport document has no verified native owner"));
    }
    let tuples = acp::zanzibar::store::TransactionZanzibarStore::new(acp_view);
    let actor = Subject::entity(runtime.clone());
    let marked = tuples
        .has_relationship(
            AGENT_OWNERSHIP_POLICY_ID,
            resource,
            doc_id,
            "local_publication",
            &actor,
        )
        .await
        .map_err(failure)?;
    if !marked {
        change(
            view,
            provider,
            runtime,
            resource,
            doc_id,
            "reader",
            actor.clone(),
            false,
        )
        .await?;
        if collection_name == PEER_REGISTRY_NAME {
            change(
                view,
                provider,
                runtime,
                resource,
                doc_id,
                "reader",
                Subject::Wildcard,
                false,
            )
            .await?;
        }
        change(
            view,
            provider,
            runtime,
            resource,
            doc_id,
            "local_publication",
            actor.clone(),
            false,
        )
        .await?;
        change(
            view,
            provider,
            runtime,
            resource,
            doc_id,
            "pending_parent",
            actor.clone(),
            false,
        )
        .await?;
    }
    let pending = tuples
        .has_relationship(
            AGENT_OWNERSHIP_POLICY_ID,
            resource,
            doc_id,
            "pending_parent",
            &actor,
        )
        .await
        .map_err(failure)?;
    if !pending {
        return Ok(());
    }
    let Some((node, owner)) = local_node(view, provider, runtime).await? else {
        return Ok(());
    };
    change(
        view,
        provider,
        runtime,
        resource,
        doc_id,
        "parent",
        Subject::entity_set("nodes", node, ""),
        false,
    )
    .await?;
    if collection_name == PEER_REGISTRY_NAME
        && document.get("node_did").and_then(|value| value.as_str()) == Some(runtime.as_str())
    {
        let creator = scoped
            .get_doc_owner(AGENT_OWNERSHIP_POLICY_ID, resource, doc_id)
            .await
            .map_err(failure)?
            .ok_or_else(|| failure("registry document has no native owner"))?;
        let writer = if creator == *runtime {
            Some(owner.clone())
        } else if creator == owner {
            Some(runtime.clone())
        } else {
            None
        };
        if let Some(writer) = writer {
            change(
                view,
                provider,
                runtime,
                resource,
                doc_id,
                "writer",
                Subject::entity(writer),
                false,
            )
            .await?;
        }
    }
    change(
        view,
        provider,
        runtime,
        resource,
        doc_id,
        "reader_manager",
        Subject::entity(owner),
        false,
    )
    .await?;
    change(
        view,
        provider,
        runtime,
        resource,
        doc_id,
        "pending_parent",
        actor,
        true,
    )
    .await?;
    Ok(())
}

/// Called on startup and local Node publication to complete only outstanding
/// native parent links. Validated scans keep concurrent inserts in the decision.
pub(crate) async fn reconcile_transport_documents(
    view: &DocumentMutationView,
    provider: &dyn DocumentACP,
    runtime: &Did,
) -> Result<()> {
    let system = view.sibling(Namespace::Systemstore);
    for name in gents_protocol::schemas::BRANCHABLE_AGENT_COLLECTION_NAMES {
        let Some(collection) =
            db::collection::loader::load_collection_from_systemstore(&system, name)
                .await
                .map_err(failure)?
        else {
            continue;
        };
        let rows = collection
            .get_all_with_datastore_validated(view, &system)
            .await
            .map_err(failure)?;
        for row in rows {
            let doc = row
                .id()
                .ok_or_else(|| failure("transport row has no document ID"))?
                .to_string();
            project_transport_document(view, provider, runtime, name, &doc).await?;
        }
    }
    Ok(())
}
