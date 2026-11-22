use acp::Subject;
use db::DocumentMutationContext;
use document::DocID;
use identity::Did;
use storage::corekv::{Error, Result};
use storage::namespace::Namespace;

use gents_protocol::schemas::{
    identity_policy_resource, is_replicated_protocol_collection, BuiltInPolicy, HomePolicyIds,
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
    context: &DocumentMutationContext<'_>,
    ids: &HomePolicyIds,
    runtime: &Did,
) -> Result<Option<(String, Did)>> {
    let view = context.view;
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
        || policy.id != ids.id(BuiltInPolicy::Household)
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
    let owner = context
        .projected_owner(ids.id(BuiltInPolicy::Household), "nodes", &doc)
        .await?
        .ok_or_else(|| failure("local Node has no registered owner"))?;
    Ok(Some((doc, owner)))
}

struct PublicationGrant<'a> {
    context: &'a DocumentMutationContext<'a>,
    collection: &'a str,
    doc: &'a str,
}

impl PublicationGrant<'_> {
    async fn change(&self, relation: &str, target: Subject, delete: bool) -> Result<bool> {
        self.context
            .change_projected_relationship(self.collection, self.doc, relation, target, delete)
            .await
    }
}

/// Stage the receiving runtime's document-scoped native grants in the accepted
/// row's transaction. A completed marker never restores a revoked grant.
pub async fn project_transport_document(
    context: &DocumentMutationContext<'_>,
    ids: &HomePolicyIds,
    collection_name: &str,
    doc_id: &str,
) -> Result<bool> {
    let view = context.view;
    let provider = context.document_acp;
    let runtime = context.node_did;
    let Some(resource) = transport_resource(collection_name) else {
        return Ok(false);
    };
    if doc_id.is_empty() {
        return Ok(false);
    }
    let system = view.sibling(Namespace::Systemstore);
    let Some(collection) =
        db::collection::loader::load_collection_from_systemstore(&system, collection_name)
            .await
            .map_err(failure)?
    else {
        return Ok(false);
    };
    let schema = collection.schema();
    if !schema.is_branchable
        || !schema.policy.as_ref().is_some_and(|policy| {
            policy.id == ids.id(BuiltInPolicy::Household) && policy.resource_name == resource
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
        return Ok(false);
    };
    let owner = context
        .projected_owner(ids.id(BuiltInPolicy::Household), resource, doc_id)
        .await?
        .ok_or_else(|| failure("transport document has no verified native owner"))?;
    let actor = Did::new(view.projection_actor().unwrap_or(runtime.as_str())).map_err(failure)?;
    if provider.canonical_actor(&owner) != provider.canonical_actor(&actor) {
        return Ok(false);
    }
    let mut changed = false;
    let grant = PublicationGrant {
        context,
        collection: collection_name,
        doc: doc_id,
    };
    let actor = Subject::entity(runtime.clone());
    let marked = context
        .projected_actor_relationship(
            ids.id(BuiltInPolicy::Household),
            resource,
            doc_id,
            "local_publication",
            runtime,
        )
        .await?;
    if !marked {
        if !provider.supports_transactions()
            && !context
                .registration_pending(ids.id(BuiltInPolicy::Household), resource, doc_id)
                .await?
        {
            return Ok(false);
        }
        changed |= grant.change("reader", actor.clone(), false).await?;
        if collection_name == PEER_REGISTRY_NAME {
            changed |= grant.change("reader", Subject::Wildcard, false).await?;
        }
        changed |= grant
            .change("local_publication", actor.clone(), false)
            .await?;
        changed |= grant.change("pending_parent", actor.clone(), false).await?;
    }
    let pending = context
        .projected_actor_relationship(
            ids.id(BuiltInPolicy::Household),
            resource,
            doc_id,
            "pending_parent",
            runtime,
        )
        .await?;
    if !pending {
        return Ok(changed);
    }
    let Some((node, owner)) = local_node(context, ids, runtime).await? else {
        return Ok(changed);
    };
    changed |= grant
        .change("parent", Subject::entity_set("nodes", node, ""), false)
        .await?;
    if collection_name == PEER_REGISTRY_NAME
        && document.get("node_did").and_then(|value| value.as_str()) == Some(runtime.as_str())
    {
        let creator = context
            .projected_owner(ids.id(BuiltInPolicy::Household), resource, doc_id)
            .await?
            .ok_or_else(|| failure("registry document has no native owner"))?;
        let writer = if creator == *runtime {
            Some(owner.clone())
        } else if creator == owner {
            Some(runtime.clone())
        } else {
            None
        };
        if let Some(writer) = writer {
            changed |= grant
                .change("writer", Subject::entity(writer), false)
                .await?;
        }
    }
    changed |= grant
        .change("reader_manager", Subject::entity(owner), false)
        .await?;
    changed |= grant.change("pending_parent", actor, true).await?;
    Ok(changed)
}

/// Called on startup and local Node publication to complete only outstanding
/// native parent links. Validated scans keep concurrent inserts in the decision.
pub async fn reconcile_transport_documents(
    context: &DocumentMutationContext<'_>,
    ids: &HomePolicyIds,
) -> Result<Vec<(String, String)>> {
    let view = context.view;
    let system = view.sibling(Namespace::Systemstore);
    let mut changed = Vec::new();
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
            if project_transport_document(context, ids, name, &doc).await? {
                changed.push(((*name).to_owned(), doc));
            }
        }
    }
    Ok(changed)
}
