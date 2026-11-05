use std::sync::Arc;

use anyhow::{bail, ensure, Context, Result};
use gents::store::config_client::{
    AgentPublicationObserver, ClientAgentJournal, ConfigAccess, ConfigClientError,
    ConfigRelationshipGrant, IdempotentTransactionRetry, PublicationStage, PublicationStatus,
};
use gents::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};

use super::{Entry, Envelope};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    attempt: String,
    journal: ClientAgentJournal,
    parent: ConfigRelationshipGrant,
    executor: ConfigRelationshipGrant,
}

impl Record {
    pub(super) fn key(&self) -> String {
        key(
            &self.journal.client,
            &self.journal.node_did,
            &self.journal.agent_did,
        )
    }

    fn terminal(&self) -> bool {
        self.journal.commit == PublicationStatus::Failed
            || (self.journal.commit == PublicationStatus::Succeeded
                && self.journal.parent == PublicationStatus::Succeeded
                && self.journal.executor == PublicationStatus::Succeeded)
    }

    pub(super) fn validate(&self) -> Result<()> {
        ensure!(!self.attempt.is_empty(), "publication attempt is missing");
        ensure!(
            matches!(&self.parent, ConfigRelationshipGrant::Resource { collection, doc_id, relation, target_resource, target_doc_id }
            if collection == "Agent" && doc_id == &self.journal.agent_doc && relation == "parent" && target_resource == "nodes" && target_doc_id == &self.journal.node_doc),
            "publication parent binding changed"
        );
        ensure!(
            matches!(&self.executor, ConfigRelationshipGrant::Actor { collection, doc_id, relation, target }
            if collection == "Agent" && doc_id == &self.journal.agent_doc && relation == "executor" && target == &self.journal.agent_did),
            "publication executor binding changed"
        );
        Ok(())
    }
}

fn key(client: &str, node: &str, agent: &str) -> String {
    let mut hash = blake3::Hasher::new();
    for part in [client, node, agent] {
        hash.update(&(part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("agent-publication-{}", hash.finalize().to_hex())
}

fn encode(record: &Record) -> Result<String> {
    Ok(escape_graphql_string(&serde_json::to_string(
        &Envelope::Publication(Box::new(record.clone())),
    )?))
}

async fn write(
    txn: &gents::store::config_client::ConfigApplyTxn<'_>,
    doc_id: &str,
    record: &Record,
) -> Result<()> {
    let doc_id = escape_graphql_string(doc_id);
    let payload = encode(record)?;
    let response = txn.execute(&format!("mutation {{ update_PendingChatSubmission(docID: \"{doc_id}\", input: {{payload: \"{payload}\"}}) {{ _docID }} }}")).await?;
    super::require_written(&response, "update_PendingChatSubmission")
}

async fn lookup(
    node: &defra_node::Node,
    client: &str,
    node_did: &str,
    agent_did: &str,
) -> Result<Option<Record>> {
    let id = key(client, node_did, agent_did);
    let response = graphql_with_transaction_retry(
        node,
        &super::query(&id),
        "desktop.agent.publication.lookup",
    )
    .await?;
    match super::row(
        response
            .data
            .as_ref()
            .context("publication query omitted data")?,
        &id,
    )? {
        None => Ok(None),
        Some((_, Entry::Publication(record))) => Ok(Some(*record)),
        Some(_) => bail!("configuration publication identity belongs to another journal"),
    }
}

async fn reserve(node: &Arc<defra_node::Node>, records: &[Record]) -> Result<()> {
    ConfigAccess::transact_local_idempotent(node, None, IdempotentTransactionRetry::Standard,
        "desktop.agent.publication.reserve", |txn| {
            Box::pin(async move {
                for record in records {
                    let id = record.key();
                    match super::read_in_txn(txn, &id).await? {
                        Some((_, Entry::Publication(existing))) if existing.attempt == record.attempt => {
                            ensure!(serde_json::to_value(&*existing)? == serde_json::to_value(record)?, "publication attempt binding changed");
                        }
                        Some((doc_id, Entry::Publication(existing))) if existing.terminal() => write(txn, &doc_id, record).await?,
                        Some((_, Entry::Publication(existing))) => bail!("Agent {} has an interrupted publication for document {}; inspect its retained journal before repair", existing.journal.agent_did, existing.journal.agent_doc),
                        Some(_) => bail!("configuration publication identity belongs to another journal"),
                        None => {
                            let escaped_id = escape_graphql_string(&id);
                            let payload = encode(record)?;
                            let response = txn.execute(&format!("mutation {{ create_PendingChatSubmission(input: {{request_id: \"{escaped_id}\", payload: \"{payload}\"}}) {{ _docID }} }}")).await?;
                            super::require_written(&response, "create_PendingChatSubmission")?;
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
        }).await
}

async fn replace(node: &Arc<defra_node::Node>, before: &[Record], after: &[Record]) -> Result<()> {
    ensure!(
        before.len() == after.len(),
        "publication update changed its extent"
    );
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "desktop.agent.publication.update",
        |txn| {
            Box::pin(async move {
                for (old, next) in before.iter().zip(after) {
                    ensure!(
                        old.key() == next.key() && old.attempt == next.attempt,
                        "publication binding changed"
                    );
                    let Some((doc_id, Entry::Publication(stored))) =
                        super::read_in_txn(txn, &old.key()).await?
                    else {
                        bail!("retained Agent publication is unavailable");
                    };
                    let stored = serde_json::to_value(&*stored)?;
                    let expected = serde_json::to_value(old)?;
                    let updated = serde_json::to_value(next)?;
                    if stored == updated {
                        continue;
                    }
                    ensure!(
                        stored == expected,
                        "retained Agent publication changed concurrently"
                    );
                    write(txn, &doc_id, next).await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
}

fn journal_error(error: anyhow::Error) -> ConfigClientError {
    ConfigClientError::ClientAgentJournal(error.into_boxed_dyn_error())
}

pub(crate) struct Observer {
    node: Arc<defra_node::Node>,
    client: String,
    bindings: Vec<(String, String)>,
    active: tokio::sync::Mutex<Vec<Record>>,
}

impl Observer {
    pub(crate) fn new(
        node: Arc<defra_node::Node>,
        client: String,
        bindings: Vec<(String, String)>,
    ) -> Arc<Self> {
        Arc::new(Self {
            node,
            client,
            bindings,
            active: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    async fn transition(
        &self,
        stage: PublicationStage,
        selected: Option<&ConfigRelationshipGrant>,
        begin: bool,
        success: bool,
        confirmed_failure: bool,
    ) -> Result<()> {
        let mut active = self.active.lock().await;
        let before = active.clone();
        let mut after = before.clone();
        let mut matched = false;
        for record in &mut after {
            if selected.is_some_and(|grant| match stage {
                PublicationStage::Parent => &record.parent != grant,
                PublicationStage::Executor => &record.executor != grant,
                PublicationStage::Commit => true,
            }) {
                continue;
            }
            matched = true;
            if begin {
                ensure!(
                    record.journal.may_record(stage),
                    "Agent {} has an interrupted {stage:?} publication",
                    record.journal.agent_did
                );
                record.journal.record_attempt(stage, true);
            } else {
                record
                    .journal
                    .finish_attempt(stage, success, confirmed_failure);
            }
        }
        ensure!(
            selected.is_none() || matched,
            "Agent publication grant has no retained binding"
        );
        replace(&self.node, &before, &after).await?;
        if begin {
            for record in &mut after {
                if record.journal.may_dispatch(stage) {
                    record.journal.dispatch(stage);
                }
            }
        }
        *active = after;
        Ok(())
    }
}

#[async_trait::async_trait]
impl AgentPublicationObserver for Observer {
    async fn before_commit(
        &self,
        creator: &str,
        grants: &[ConfigRelationshipGrant],
    ) -> Result<(), ConfigClientError> {
        super::require_client_signer(&self.node, &self.client).map_err(journal_error)?;
        let build = || -> Result<Vec<Record>> {
            ensure!(
                creator == self.client,
                "Agent publication creator does not match the client"
            );
            let mut records = Vec::new();
            for executor in grants {
                let ConfigRelationshipGrant::Actor {
                    collection,
                    doc_id,
                    relation,
                    target,
                } = executor
                else {
                    continue;
                };
                if collection != "Agent" || relation != "executor" {
                    continue;
                }
                let node_did = self
                    .bindings
                    .iter()
                    .find(|(agent, _)| agent == target)
                    .map(|(_, node)| node)
                    .context("Agent publication has no logical binding")?;
                let parent = grants.iter().find(|grant| matches!(grant, ConfigRelationshipGrant::Resource { collection, doc_id: parent_doc, relation, target_resource, .. } if collection == "Agent" && parent_doc == doc_id && relation == "parent" && target_resource == "nodes"))
                    .context("Agent publication has no Node parent")?;
                let ConfigRelationshipGrant::Resource { target_doc_id, .. } = parent else {
                    unreachable!()
                };
                let mut journal = ClientAgentJournal::new(
                    creator.to_owned(),
                    target.clone(),
                    node_did.clone(),
                    doc_id.clone(),
                    target_doc_id.clone(),
                );
                journal.record_attempt(PublicationStage::Commit, true);
                ensure!(
                    journal.may_dispatch(PublicationStage::Commit),
                    "invalid Agent publication binding"
                );
                let record = Record {
                    attempt: uuid::Uuid::new_v4().to_string(),
                    journal,
                    parent: parent.clone(),
                    executor: executor.clone(),
                };
                record.validate()?;
                records.push(record);
            }
            Ok(records)
        };
        let mut records = build().map_err(journal_error)?;
        reserve(&self.node, &records).await.map_err(journal_error)?;
        for record in &mut records {
            record.journal.dispatch(PublicationStage::Commit);
        }
        *self.active.lock().await = records;
        Ok(())
    }

    async fn committed(&self) -> Result<(), ConfigClientError> {
        self.transition(PublicationStage::Commit, None, false, true, false)
            .await
            .map_err(journal_error)
    }

    async fn commit_failed(&self, definitely_not_committed: bool) -> Result<(), ConfigClientError> {
        self.transition(
            PublicationStage::Commit,
            None,
            false,
            false,
            definitely_not_committed,
        )
        .await
        .map_err(journal_error)
    }

    async fn before_grant(&self, grant: &ConfigRelationshipGrant) -> Result<(), ConfigClientError> {
        let stage = match grant {
            ConfigRelationshipGrant::Resource {
                collection,
                relation,
                ..
            } if collection == "Agent" && relation == "parent" => PublicationStage::Parent,
            ConfigRelationshipGrant::Actor {
                collection,
                relation,
                ..
            } if collection == "Agent" && relation == "executor" => PublicationStage::Executor,
            _ => return Ok(()),
        };
        self.transition(stage, Some(grant), true, false, false)
            .await
            .map_err(journal_error)
    }

    async fn granted(&self, grant: &ConfigRelationshipGrant) -> Result<(), ConfigClientError> {
        let stage = match grant {
            ConfigRelationshipGrant::Resource {
                collection,
                relation,
                ..
            } if collection == "Agent" && relation == "parent" => PublicationStage::Parent,
            ConfigRelationshipGrant::Actor {
                collection,
                relation,
                ..
            } if collection == "Agent" && relation == "executor" => PublicationStage::Executor,
            _ => return Ok(()),
        };
        self.transition(stage, Some(grant), false, true, false)
            .await
            .map_err(journal_error)
    }
}

pub(crate) async fn ensure_recovered(
    node: &Arc<defra_node::Node>,
    access: &ConfigAccess,
    client: &str,
    node_did: &str,
    agent_did: &str,
) -> Result<()> {
    super::require_client_signer(node, client)?;
    ensure!(
        access.actor_did().as_deref() == Some(client),
        "Agent publication actor changed"
    );
    let Some(record) = lookup(node, client, node_did, agent_did).await? else {
        return Ok(());
    };
    ensure!(
        record.journal.client == client
            && record.journal.node_did == node_did
            && record.journal.agent_did == agent_did,
        "retained Agent publication identity changed"
    );
    if record.journal.commit == PublicationStatus::Failed || record.terminal() {
        return Ok(());
    }
    ensure!(record.journal.commit == PublicationStatus::Succeeded,
        "Agent {agent_did} creation for document {} has an unknown outcome; inspect its retained publication before repair", record.journal.agent_doc);
    bail!("Agent {agent_did} document {} has unfinished relationships (parent: {:?}, executor: {:?}); current parent and ownership bindings cannot be verified atomically, so inspect and repair them explicitly", record.journal.agent_doc, record.journal.parent, record.journal.executor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matching_attempt_id_cannot_replace_physical_grant_bindings() -> Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        let mut journal = ClientAgentJournal::new(
            node.node_identity_did().context("client identity")?.into(),
            "agent".into(),
            "node".into(),
            "agent-doc".into(),
            "node-doc".into(),
        );
        journal.record_attempt(PublicationStage::Commit, true);
        let record = Record {
            attempt: "same-attempt".into(),
            journal,
            parent: ConfigRelationshipGrant::Resource {
                collection: "Agent".into(),
                doc_id: "agent-doc".into(),
                relation: "parent".into(),
                target_resource: "nodes".into(),
                target_doc_id: "node-doc".into(),
            },
            executor: ConfigRelationshipGrant::Actor {
                collection: "Agent".into(),
                doc_id: "agent-doc".into(),
                relation: "executor".into(),
                target: "agent".into(),
            },
        };
        record.validate()?;
        reserve(&node, std::slice::from_ref(&record)).await?;
        reserve(&node, std::slice::from_ref(&record)).await?;
        let mut different = record.clone();
        different.journal.node_doc = "different-node-doc".into();
        if let ConfigRelationshipGrant::Resource { target_doc_id, .. } = &mut different.parent {
            *target_doc_id = different.journal.node_doc.clone();
        }
        different.validate()?;
        assert!(reserve(&node, &[different]).await.is_err());
        let retained = lookup(&node, "client", "node", "agent")
            .await?
            .context("retained record")?;
        assert_eq!(
            serde_json::to_value(retained)?,
            serde_json::to_value(record)?
        );
        node.shutdown().await?;
        Ok(())
    }
}
