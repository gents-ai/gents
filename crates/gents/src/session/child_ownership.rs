use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::graphql::escape_graphql_string;

#[cfg(test)]
#[path = "child_ownership/auxiliary_repair_tests.rs"]
mod auxiliary_repair_tests;

const CHILD_COLLECTIONS: [&str; 5] = [
    "AgentMessage",
    "AgentOutputSegment",
    "AgentToolCall",
    "Goal",
    "GoalCreationClaim",
];

#[derive(Debug, Deserialize)]
struct Child {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_doc_id: String,
    node_did: String,
    session_id: String,
    requester_did: Option<String>,
    agent_did: String,
    creator_did: String,
    #[serde(default)]
    request_doc_id: Option<String>,
    #[serde(default)]
    source: Option<gents_protocol::output::OutputSource>,
}

#[derive(Debug, Deserialize)]
struct Session {
    #[serde(rename = "_docID")]
    doc_id: String,
    node_did: String,
    session_id: String,
    requester_did: Option<String>,
    agent_did: String,
}

struct PendingChild {
    collection: &'static str,
    doc_id: String,
    session_doc_id: String,
}

/// The child is created before its relationships; each pending grant and its
/// marker commit together, so completed publication cannot restore a revocation.
pub(crate) async fn recover_agent_session_child_relationships_with_access(
    access: &ConfigAccess,
    node_did: &str,
    agent: &identity::Did,
) -> Result<usize> {
    recover_agent_children(access, node_did, agent, None).await
}

pub(crate) async fn publish_agent_session_child_relationships_with_access(
    access: &ConfigAccess,
    node_did: &str,
    agent: &identity::Did,
    session_id: &str,
) -> Result<usize> {
    recover_agent_children(access, node_did, agent, Some(session_id)).await
}

async fn recover_agent_children(
    access: &ConfigAccess,
    node_did: &str,
    agent: &identity::Did,
    session_id: Option<&str>,
) -> Result<usize> {
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(agent.as_str()),
        "child repair actor differs from access actor"
    );
    let expected_agent = agent.to_string();
    let node_did = node_did.to_owned();
    let session_id = session_id.map(str::to_owned);
    let pending = access
        .transact_readonly("session.recover_child_relationships", |txn| {
            let expected_agent = expected_agent.clone();
            let node_did = node_did.clone();
            let session_id = session_id.clone();
            Box::pin(async move {
                load_pending(txn, &node_did, &expected_agent, session_id.as_deref(), None).await
            })
        })
        .await?;
    let mut repaired = 0;
    for child in pending {
        let published = access
            .transact_write("session.publish_child_relationships", |txn| {
                let child = &child;
                let expected_agent = &expected_agent;
                let node_did = &node_did;
                let session_id = &session_id;
                Box::pin(async move {
                    let current = load_pending(
                        txn, node_did, expected_agent, session_id.as_deref(), Some(child),
                    ).await?;
                    if current.is_empty() {
                        return Ok::<bool, anyhow::Error>(false);
                    }
                    anyhow::ensure!(current.len() == 1, "ambiguous child relationship recovery");
                    txn.add_resource_relationship(
                        child.collection, &child.doc_id, "parent", "sessions", &child.session_doc_id,
                    ).await?;
                    if matches!(child.collection, "Goal" | "GoalCreationClaim") {
                        txn.add_actor_relationship(child.collection, &child.doc_id, "working_agent", agent).await?;
                    }
                    let doc = escape_graphql_string(&child.doc_id);
                    let creator = escape_graphql_string(agent.as_str());
                    let session = escape_graphql_string(&child.session_doc_id);
                    let collection = child.collection;
                    let response = txn.execute(&format!(
                        r#"mutation {{ update_{collection}(docID: "{doc}", filter: {{ creator_did: {{ _eq: "{creator}" }}, session_doc_id: {{ _eq: "{session}" }}, parent_linked: {{ _eq: false }} }}, input: {{ parent_linked: true }}) {{ _docID }} }}"#
                    )).await?;
                    let updated = crate::store::graphql::mutation_documents(
                        &response, &format!("update_{collection}"),
                    )?.context("child relationship marker omitted mutation result")?;
                    anyhow::ensure!(
                        updated.len() == 1 && updated[0]["_docID"].as_str() == Some(child.doc_id.as_str()),
                        "child relationship marker lost its owner-bound compare-and-set"
                    );
                    Ok(true)
                })
            })
            .await?;
        repaired += usize::from(published);
    }

    Ok(repaired)
}

async fn load_pending(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent: &str,
    session_id: Option<&str>,
    only_child: Option<&PendingChild>,
) -> Result<Vec<PendingChild>> {
    let mut filter = format!(
        r#"node_did: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, creator_did: {{ _eq: "{}" }}, parent_linked: {{ _eq: false }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(agent),
        escape_graphql_string(agent)
    );
    if let Some(session_id) = session_id {
        filter.push_str(&format!(
            r#", session_id: {{ _eq: "{}" }}"#,
            escape_graphql_string(session_id)
        ));
    }
    if let Some(child) = only_child {
        filter.push_str(&format!(
            r#", _docID: {{ _eq: "{}" }}"#,
            escape_graphql_string(&child.doc_id)
        ));
    }
    let mut pending = Vec::new();
    for collection in CHILD_COLLECTIONS {
        if only_child.is_some_and(|child| child.collection != collection) {
            continue;
        }
        let fields = if collection == "AgentOutputSegment" {
            "_docID session_doc_id node_did session_id requester_did agent_did creator_did request_doc_id source"
        } else {
            "_docID session_doc_id node_did session_id requester_did agent_did creator_did"
        };
        let rows = txn.cursor_rows(collection, fields, &filter).await?;
        for chunk in rows.chunks(64) {
            let children = chunk.iter().filter_map(|row| match serde_json::from_value::<Child>(row.clone()) {
                Ok(child) => Some(child),
                Err(error) => {
                    tracing::warn!(collection, %error, "invalid session child recovery metadata");
                    None
                }
            }).collect::<Vec<_>>();
            if children.is_empty() {
                continue;
            }
            let ids = children
                .iter()
                .map(|row| row.doc_id.clone())
                .collect::<Vec<_>>();
            let owners = txn.document_owners(collection, &ids).await?;
            anyhow::ensure!(
                owners.len() == children.len(),
                "child ownership observation is incomplete"
            );
            let mut owner_by_id = BTreeMap::new();
            for owner in owners {
                anyhow::ensure!(
                    owner_by_id.insert(owner.doc_id.clone(), owner).is_none(),
                    "duplicate child ownership observation"
                );
            }
            for child in children {
                let Some(owner) = owner_by_id.get(&child.doc_id) else {
                    continue;
                };
                if !owner.visible
                    || owner.owner.as_ref().is_none_or(|did| did.as_str() != agent)
                    || child.creator_did != agent
                    || child.agent_did != agent
                    || child.node_did != node_did
                {
                    continue;
                }
                let parent = escape_graphql_string(&child.session_doc_id);
                let response = txn.execute(&format!(r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{parent}" }} }}, limit: 2) {{ _docID node_did session_id requester_did agent_did }} }}"#)).await?;
                let parents = response["data"]["AgentSession"]
                    .as_array()
                    .context("child parent query omitted rows")?;
                if parents.len() != 1 {
                    continue;
                }
                let session: Session = serde_json::from_value(parents[0].clone())?;
                if only_child
                    .is_some_and(|expected| expected.session_doc_id != child.session_doc_id)
                    || session.doc_id != child.session_doc_id
                    || session.node_did != child.node_did
                    || session.session_id != child.session_id
                    || session.agent_did != child.agent_did
                {
                    continue;
                }
                let auxiliary = match (&child.source, collection) {
                    (
                        Some(gents_protocol::output::OutputSource::ProviderTurn { scope, .. }),
                        "AgentOutputSegment",
                    ) => scope.kind.request_purpose().is_auxiliary(),
                    (None, "AgentOutputSegment") => continue,
                    _ => false,
                };
                if auxiliary {
                    let (Some(request_doc), Some(source)) = (&child.request_doc_id, &child.source)
                    else {
                        continue;
                    };
                    let binding =
                        crate::session::canonical_rows::session_child_binding_for_request_in_txn(
                            txn,
                            &child.node_did,
                            &child.session_id,
                            child.requester_did.as_deref(),
                            request_doc,
                            crate::session::canonical_rows::SessionChildSource::Output(source),
                        )
                        .await;
                    if !matches!(binding, Ok((ref physical, ref selected)) if physical == &child.session_doc_id && selected == agent)
                    {
                        continue;
                    }
                } else if session.requester_did != child.requester_did {
                    continue;
                }
                pending.push(PendingChild {
                    collection,
                    doc_id: child.doc_id,
                    session_doc_id: child.session_doc_id,
                });
            }
        }
    }
    Ok(pending)
}

#[cfg(test)]
mod tests;
