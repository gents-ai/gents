//! Lean `Identity.AgentTargetAccess` and `Identity.DeclaredAccess`: an Agent
//! whose AgentContext selects Tools with an enabled agents group, or with a
//! plugin job block naming the target, declares itself a `session_writer` of
//! each such selected AgentTarget's Agent on the same Node. The configuring
//! actor applies exactly the changed tuples in the configuration
//! transaction, so native ACP bounds them by that actor's
//! authority and a refused tuple refuses the whole apply. An Agent the plan
//! removes has its declared tuples revoked before it is deleted.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use gents_protocol::graphql::graphql_rows_from_response;
use serde_json::Value;

use super::{plan::document_identity, projection::config_projection, DesiredStateApplyPlan};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;

const RELATION: &str = "session_writer";

/// Each Agent document on one Node, keyed by Agent DID, with its declared
/// session writers.
type Declared = BTreeMap<String, (String, BTreeSet<String>)>;

#[derive(Default)]
pub(super) struct Capture(BTreeMap<String, Declared>);

fn declares(collection: Collection) -> bool {
    matches!(
        collection,
        Collection::Agent | Collection::AgentContext | Collection::Tools | Collection::AgentTarget
    )
}

async fn rows(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
) -> Result<Vec<(String, Value)>> {
    let name = collection.graphql_type();
    let fields = config_projection(collection, None)?.0.join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ {name}(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID {fields} }} }}"#,
            escape_graphql_string(owner)
        ))
        .await?;
    graphql_rows_from_response(&response, name)
        .into_iter()
        .map(|mut row| {
            let doc = row
                .as_object_mut()
                .and_then(|row| row.remove("_docID"))
                .and_then(|doc| doc.as_str().map(str::to_owned))
                .with_context(|| format!("{name} row missing physical ID"))?;
            let value = config_projection(collection, Some(&row))?
                .1
                .with_context(|| format!("{name} canonical projection missing"))?;
            Ok((doc, value))
        })
        .collect()
}

fn text<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value[field].as_str().filter(|text| !text.is_empty())
}

fn strings(value: &Value) -> impl Iterator<Item = &str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// Lean `AgentTargetAccess.startable`: an enabled agents group's target ids,
/// and otherwise those a plugin job block's `agent_targets` also names.
fn startable(tools: &Value) -> Vec<String> {
    let group = &tools["agents"];
    let enabled = group["enabled"] == Value::Bool(true);
    let jobs: BTreeSet<&str> = tools["integrations"]["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|plugin| strings(&plugin["job"]["agent_targets"]))
        .collect();
    strings(&group["target_ids"])
        .filter(|id| enabled || jobs.contains(id))
        .map(str::to_owned)
        .collect()
}

/// Lean `AgentTargetAccess.writers` over the Node's stored configuration.
async fn declared(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Declared> {
    let contexts: BTreeMap<String, String> = rows(txn, Collection::AgentContext, owner)
        .await?
        .into_iter()
        .filter_map(|(_, context)| {
            Some((
                text(&context, "context_id")?.to_owned(),
                text(&context, "tools_id")?.to_owned(),
            ))
        })
        .collect();
    let tools: BTreeMap<String, Vec<String>> = rows(txn, Collection::Tools, owner)
        .await?
        .into_iter()
        .filter_map(|(_, tools)| Some((text(&tools, "tools_id")?.to_owned(), startable(&tools))))
        .collect();
    let targets: BTreeMap<String, String> = rows(txn, Collection::AgentTarget, owner)
        .await?
        .into_iter()
        .filter(|(_, target)| text(target, "target_node_did") == Some(owner))
        .filter_map(|(_, target)| {
            Some((
                text(&target, "target_id")?.to_owned(),
                text(&target, "agent_did")?.to_owned(),
            ))
        })
        .collect();
    let agents = rows(txn, Collection::Agent, owner).await?;
    let mut declared: Declared = agents
        .iter()
        .filter_map(|(doc, agent)| {
            Some((
                text(agent, "agent_did")?.to_owned(),
                (doc.clone(), BTreeSet::new()),
            ))
        })
        .collect();
    for (_, agent) in &agents {
        let Some(writer) = text(agent, "agent_did") else {
            continue;
        };
        let selected = text(agent, "context_id")
            .and_then(|context| contexts.get(context))
            .and_then(|tools_id| tools.get(tools_id))
            .into_iter()
            .flatten()
            .filter_map(|target_id| targets.get(target_id));
        for target in selected {
            if let Some((_, writers)) = declared.get_mut(target) {
                writers.insert(writer.to_owned());
            }
        }
    }
    Ok(declared)
}

async fn change(
    txn: &ConfigApplyTxn<'_>,
    target: &str,
    doc: &str,
    writers: impl IntoIterator<Item = &String>,
    add: bool,
) -> Result<()> {
    for writer in writers {
        let did = identity::Did::new(writer)?;
        if add {
            txn.add_actor_relationship("Agent", doc, RELATION, &did).await
        } else {
            txn.delete_actor_relationship("Agent", doc, RELATION, &did)
                .await
        }
        .with_context(|| {
            format!(
                "the configuring actor cannot {} {RELATION} for {writer} on Agent {target}, which its agent target selection declares",
                if add { "grant" } else { "revoke" }
            )
        })?;
    }
    Ok(())
}

/// Records each affected Node's declaration and revokes the declared writers
/// of each Agent the plan removes, while it still exists.
pub(super) async fn capture(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<Capture> {
    let mut owners = BTreeSet::new();
    for document in plan.documents() {
        if declares(document.collection) {
            owners.insert(document_identity(document.collection, &document.add)?.0);
        }
    }
    owners.extend(
        plan.removals()
            .iter()
            .filter(|(collection, _, _)| declares(*collection))
            .map(|(_, owner, _)| owner.as_str()),
    );
    let mut before = BTreeMap::new();
    for owner in owners {
        let mut prior = declared(txn, owner).await?;
        for (_, _, agent) in plan
            .removals()
            .iter()
            .filter(|(collection, removed_owner, _)| {
                *collection == Collection::Agent && removed_owner == owner
            })
        {
            if let Some((doc, writers)) = prior.remove(agent) {
                change(txn, agent, &doc, &writers, false).await?;
            }
        }
        before.insert(owner.to_owned(), prior);
    }
    Ok(Capture(before))
}

/// Revokes withdrawn and grants added writers on each Agent of each affected
/// Node. An Agent this plan created whose Node parent is published after
/// commit receives its grants after that parent, through the same queue.
pub(super) async fn apply(
    txn: &ConfigApplyTxn<'_>,
    before: Capture,
    queued_agents: &BTreeSet<String>,
) -> Result<()> {
    for (owner, prior) in before.0 {
        let next = declared(txn, &owner).await?;
        for (target, (doc, writers)) in &next {
            let empty = BTreeSet::new();
            let old = prior.get(target).map_or(&empty, |(_, writers)| writers);
            change(txn, target, doc, old.difference(writers), false).await?;
            if queued_agents.contains(doc) {
                for writer in writers.difference(old) {
                    identity::Did::new(writer)?;
                    txn.queue_relationship_grant(
                        gents_store::config_client::ConfigRelationshipGrant::Actor {
                            collection: "Agent".to_owned(),
                            doc_id: doc.clone(),
                            relation: RELATION.to_owned(),
                            target: writer.clone(),
                        },
                    )?;
                }
            } else {
                change(txn, target, doc, writers.difference(old), true).await?;
            }
        }
    }
    Ok(())
}
