use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result};
use gents_store::collection::Collection;
use gents_store::config_client::body_verification::{
    body_verification, ClosureDocument, MergeCheck, PlacementBodies,
};
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::{document_composite_version, escape_graphql_string};
use serde_json::Value;

use super::read::read_record;
use crate::references_of;

struct Selected {
    doc_id: String,
    head: String,
    value: Value,
}

async fn selected(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<Selected>> {
    let Some((doc_id, value)) = read_record(txn, collection, owner, id).await? else {
        return Ok(None);
    };
    let name = collection.graphql_type();
    let response = txn
        .execute(&format!(
            r#"{{ {name}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _version {{ cid height fieldName }} }} }}"#,
            escape_graphql_string(&doc_id)
        ))
        .await?;
    let row = response
        .pointer(&format!("/data/{name}/0"))
        .with_context(|| format!("{name} {doc_id} has no version row"))?;
    let head = document_composite_version(row, "body verification")?
        .with_context(|| format!("{name} {doc_id} has no composite version"))?
        .cid;
    Ok(Some(Selected {
        doc_id,
        head,
        value,
    }))
}

/// The Agent document and every document its configuration selects, the
/// Agent first. Another Agent a target names is a body of its own and is not
/// followed.
async fn closure(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    agent_did: &str,
) -> Result<Vec<(Collection, Option<Selected>)>> {
    let mut closure = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([(Collection::Agent, agent_did.to_owned())]);
    while let Some((collection, id)) = queue.pop_front() {
        if !seen.insert((collection, id.clone())) {
            continue;
        }
        let document = selected(txn, collection, owner, &id).await?;
        if let Some(document) = &document {
            queue.extend(
                references_of(collection, &document.value, owner)?
                    .into_iter()
                    .filter(|reference| reference.collection != Collection::Agent)
                    .map(|reference| (reference.collection, reference.id)),
            );
        }
        closure.push((collection, document));
    }
    Ok(closure)
}

/// Whether the body of each of `owner`'s Agents is authorized, by Agent
/// `_docID`, read in `txn`. `agents` names each Agent's DID and `_docID`. A
/// document that cannot be read counts as absent and a registration the
/// provider does not answer as not registered: the body waits.
pub async fn placement_bodies(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    agents: &[(String, String)],
) -> Result<PlacementBodies> {
    let check = txn.merge_check();
    let issuer_home = txn.home_policy().unwrap_or_default();
    let reads_closure = matches!(&check, MergeCheck::Strict { home } if home == issuer_home);
    let mut registered = BTreeMap::new();
    let mut bodies = PlacementBodies::new();
    for (agent_did, agent_doc) in agents {
        let mut documents = Vec::new();
        if reads_closure {
            let closure = closure(txn, owner, agent_did)
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(agent_did, %error, "a body document was not read; the body waits");
                    vec![(Collection::Agent, None)]
                });
            for (collection, document) in closure {
                let Some(document) = document else {
                    documents.push(ClosureDocument {
                        present: false,
                        registered: false,
                        head: String::new(),
                    });
                    continue;
                };
                let key = (collection, document.doc_id.clone());
                let is_registered = match registered.get(&key) {
                    Some(known) => *known,
                    None => {
                        let read = txn
                            .document_registered(issuer_home, collection, &document.doc_id)
                            .await
                            .unwrap_or_else(|error| {
                                tracing::warn!(
                                    collection = collection.graphql_type(),
                                    doc_id = document.doc_id,
                                    %error,
                                    "registration read was not answered; the body waits"
                                );
                                false
                            });
                        registered.insert(key, read);
                        read
                    }
                };
                documents.push(ClosureDocument {
                    present: true,
                    registered: is_registered,
                    head: document.head,
                });
            }
        }
        bodies.insert(
            agent_doc.clone(),
            body_verification(&check, issuer_home, &documents),
        );
    }
    Ok(bodies)
}
