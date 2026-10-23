use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::file_inventory::{
    FileInventoryAdmission, FileInventoryPageDemand, FileInventoryRefusalReceipt,
    FileInventoryReply,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::{escape_graphql_string, required_rows};
use crate::NodeIdentity;

use super::{FileInventoryPrefix, FileInventoryReceiver};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientInventoryAttempt {
    attempt_id: String,
    receiver: FileInventoryReceiver,
    refusal: Option<FileInventoryRefusalReceipt>,
}

impl ClientInventoryAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn receiver(&self) -> &FileInventoryReceiver {
        &self.receiver
    }

    pub fn refusal(&self) -> Option<&FileInventoryRefusalReceipt> {
        self.refusal.as_ref()
    }

    fn demand(&self) -> FileInventoryPageDemand {
        FileInventoryPageDemand {
            admission: self.receiver.admission.clone(),
            attempt_id: self.attempt_id.clone(),
            cursor: self
                .receiver
                .pages
                .last()
                .and_then(|page| page.receipt.page.next.clone()),
        }
    }

    async fn validated(&self, verifier: &dyn NodeIdentity) -> Result<Option<FileInventoryPrefix>> {
        ensure!(
            !self.attempt_id.is_empty(),
            "file inventory attempt is empty"
        );
        let prefix = self.receiver.validated(verifier).await?;
        let mut demand = FileInventoryPageDemand {
            admission: self.receiver.admission.clone(),
            attempt_id: self.attempt_id.clone(),
            cursor: None,
        };
        demand.validate_reference()?;
        for page in &self.receiver.pages {
            demand.validate_page_reference(&page.receipt)?;
            demand.cursor = page.receipt.page.next.clone();
        }
        if let Some(refusal) = &self.refusal {
            ensure!(
                !prefix.as_ref().is_some_and(FileInventoryPrefix::complete),
                "completed file inventory contains a refusal"
            );
            super::verify_reply(
                &demand,
                &FileInventoryReply::Refused(refusal.clone()),
                verifier,
            )
            .await?;
        }
        Ok(prefix)
    }

    pub async fn pending_demand(
        &self,
        verifier: &dyn NodeIdentity,
    ) -> Result<Option<FileInventoryPageDemand>> {
        let prefix = self.validated(verifier).await?;
        Ok(
            (self.refusal.is_none() && !prefix.is_some_and(|prefix| prefix.complete()))
                .then(|| self.demand()),
        )
    }
}

/// Progress is local observation state, not another session or access policy.
/// Persisted signatures are rechecked on every load; prior complete pages are
/// retained during refresh without claiming they describe the new attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientInventoryProgress {
    active: ClientInventoryAttempt,
    previous: Option<ClientInventoryAttempt>,
    retired_attempts: Vec<String>,
}

impl ClientInventoryProgress {
    pub fn new(admission: FileInventoryAdmission, attempt_id: String) -> Result<Self> {
        FileInventoryPageDemand {
            admission: admission.clone(),
            attempt_id: attempt_id.clone(),
            cursor: None,
        }
        .validate_reference()?;
        Ok(Self {
            active: ClientInventoryAttempt {
                attempt_id,
                receiver: FileInventoryReceiver {
                    admission,
                    pages: Vec::new(),
                },
                refusal: None,
            },
            previous: None,
            retired_attempts: Vec::new(),
        })
    }

    pub fn active(&self) -> &ClientInventoryAttempt {
        &self.active
    }

    pub fn previous(&self) -> Option<&ClientInventoryAttempt> {
        self.previous.as_ref()
    }

    pub fn admission(&self) -> &FileInventoryAdmission {
        &self.active.receiver.admission
    }

    pub async fn validated(
        &self,
        verifier: &dyn NodeIdentity,
    ) -> Result<Option<FileInventoryPrefix>> {
        let mut retired = rapidhash::RapidHashSet::default();
        ensure!(
            self.retired_attempts
                .iter()
                .all(|id| !id.is_empty() && retired.insert(id))
                && !retired.contains(&self.active.attempt_id),
            "file inventory client attempt retirement is invalid"
        );
        if let Some(previous) = &self.previous {
            ensure!(
                previous.receiver.admission == *self.admission()
                    && retired.contains(&previous.attempt_id)
                    && previous.refusal.is_none()
                    && previous
                        .validated(verifier)
                        .await?
                        .is_some_and(|prefix| prefix.complete()),
                "previous file inventory is not a completed view of this session"
            );
        }
        self.active.validated(verifier).await
    }

    pub async fn restart(&self, attempt: &str, verifier: &dyn NodeIdentity) -> Result<Self> {
        let prefix = self.validated(verifier).await?;
        let mut next = Self::new(self.admission().clone(), attempt.to_owned())?;
        if self.active.attempt_id == attempt {
            return Ok(self.clone());
        }
        ensure!(
            !self
                .retired_attempts
                .iter()
                .any(|retired| retired == attempt),
            "file inventory attempt was already superseded"
        );
        next.retired_attempts = self.retired_attempts.clone();
        next.retired_attempts.push(self.active.attempt_id.clone());
        next.previous = if prefix.is_some_and(|prefix| prefix.complete()) {
            Some(self.active.clone())
        } else {
            self.previous.clone()
        };
        Ok(next)
    }

    pub async fn receive(
        &self,
        demand: &FileInventoryPageDemand,
        reply: &FileInventoryReply,
        verifier: &dyn NodeIdentity,
    ) -> Result<Self> {
        self.validated(verifier).await?;
        ensure!(
            demand.admission == *self.admission() && demand.attempt_id == self.active.attempt_id,
            "file inventory reply belongs to another client attempt"
        );
        super::verify_reply(demand, reply, verifier).await?;
        match reply {
            FileInventoryReply::Page(receipt)
                if self.active.receiver.pages.iter().any(|page| {
                    page.request_key == receipt.request_key && page.receipt == *receipt
                }) =>
            {
                return Ok(self.clone())
            }
            FileInventoryReply::Refused(receipt)
                if self.active.refusal.as_ref() == Some(receipt) =>
            {
                return Ok(self.clone());
            }
            _ => {}
        }
        ensure!(
            self.active.pending_demand(verifier).await?.as_ref() == Some(demand),
            "file inventory reply is not the pending continuation"
        );
        let mut next = self.clone();
        match reply {
            FileInventoryReply::Page(receipt) => {
                next.active.receiver = self
                    .active
                    .receiver
                    .receive_for_demand(demand, receipt.clone(), verifier)
                    .await?;
            }
            FileInventoryReply::Refused(receipt) => next.active.refusal = Some(receipt.clone()),
        }
        Ok(next)
    }
}

fn progress_key(node: &EmbeddedNode, admission: &FileInventoryAdmission) -> Result<String> {
    let owner = node
        .node_identity_did()
        .context("file inventory client identity is unavailable")?;
    let admission = serde_json::to_string(admission)?;
    let payload = gents_protocol::enrollment::canonical_domain_payload(
        "gents-file-inventory-client-progress-v1",
        [owner, admission.as_str()],
    );
    Ok(bs58::encode(Sha256::digest(payload)).into_string())
}

#[derive(Deserialize)]
struct ProgressRow {
    #[serde(rename = "_docID")]
    document_id: String,
    progress_key: String,
    client_did: String,
    progress: ClientInventoryProgress,
}

async fn load_in_txn(
    txn: &ConfigApplyTxn<'_>,
    key: &str,
    owner: &str,
    admission: &FileInventoryAdmission,
    verifier: &dyn NodeIdentity,
) -> Result<Option<ProgressRow>> {
    let response = txn
        .execute_local_response(&format!(
            r#"{{ FileInventoryProgress(filter: {{ progress_key: {{ _eq: "{}" }} }}, limit: 2) {{
        _docID progress_key client_did progress }} }}"#,
            escape_graphql_string(key)
        ))
        .await?;
    let mut rows = required_rows::<ProgressRow>(&response, "FileInventoryProgress")?;
    ensure!(
        rows.len() <= 1,
        "file inventory client progress is ambiguous"
    );
    let Some(row) = rows.pop() else {
        return Ok(None);
    };
    ensure!(
        row.progress_key == key && row.client_did == owner && row.progress.admission() == admission,
        "file inventory client progress crossed its scope"
    );
    let owners = txn
        .document_owners(
            "FileInventoryProgress",
            std::slice::from_ref(&row.document_id),
        )
        .await?;
    ensure!(
        owners.len() == 1
            && owners[0].doc_id == row.document_id
            && owners[0]
                .owner
                .as_ref()
                .is_some_and(|did| did.as_str() == owner),
        "file inventory client progress is not owned by this client"
    );
    row.progress.validated(verifier).await?;
    Ok(Some(row))
}

pub async fn load_client_progress(
    node: &Arc<EmbeddedNode>,
    admission: &FileInventoryAdmission,
    verifier: &dyn NodeIdentity,
) -> Result<Option<ClientInventoryProgress>> {
    let key = progress_key(node, admission)?;
    let owner = node
        .node_identity_did()
        .context("file inventory client identity is unavailable")?;
    ConfigAccess::transact_local_readonly(node, None, "file_inventory.load_client", |txn| {
        let key = key.as_str();
        Box::pin(async move {
            Ok(load_in_txn(txn, key, owner, admission, verifier)
                .await?
                .map(|row| row.progress))
        })
    })
    .await
}

pub async fn begin_client_progress(
    node: &Arc<EmbeddedNode>,
    admission: &FileInventoryAdmission,
    attempt_id: &str,
    verifier: &dyn NodeIdentity,
) -> Result<ClientInventoryProgress> {
    let key = progress_key(node, admission)?;
    let owner = node
        .node_identity_did()
        .context("file inventory client identity is unavailable")?;
    ConfigAccess::transact_local_write(node, None, "file_inventory.begin_client", |txn| {
        let key = key.as_str();
        Box::pin(async move {
            let prior = load_in_txn(txn, key, owner, admission, verifier).await?;
            let next = match &prior {
                Some(row) => row.progress.restart(attempt_id, verifier).await?,
                None => ClientInventoryProgress::new(admission.clone(), attempt_id.to_owned())?,
            };
            save_in_txn(txn, key, owner, prior.as_ref(), &next).await?;
            Ok(next)
        })
    })
    .await
}

pub async fn receive_client_reply(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
    reply: &FileInventoryReply,
    verifier: &dyn NodeIdentity,
) -> Result<ClientInventoryProgress> {
    ConfigAccess::transact_local_write(node, None, "file_inventory.receive_client", |txn| {
        Box::pin(receive_in_txn(txn, node, demand, reply, verifier))
    })
    .await
}

pub(super) async fn receive_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node: &EmbeddedNode,
    demand: &FileInventoryPageDemand,
    reply: &FileInventoryReply,
    verifier: &dyn NodeIdentity,
) -> Result<ClientInventoryProgress> {
    let key = progress_key(node, &demand.admission)?;
    let owner = node
        .node_identity_did()
        .context("file inventory client identity is unavailable")?;
    let row = load_in_txn(txn, &key, owner, &demand.admission, verifier)
        .await?
        .context("file inventory attempt must be saved before transport")?;
    let next = row.progress.receive(demand, reply, verifier).await?;
    save_in_txn(txn, &key, owner, Some(&row), &next).await?;
    Ok(next)
}

async fn save_in_txn(
    txn: &ConfigApplyTxn<'_>,
    key: &str,
    owner: &str,
    prior: Option<&ProgressRow>,
    progress: &ClientInventoryProgress,
) -> Result<()> {
    if prior.is_some_and(|prior| prior.progress == *progress) {
        return Ok(());
    }
    let result = match prior {
        None => txn.execute_with_variables(
            "mutation($input: FileInventoryProgressMutationInputArg!) { create_FileInventoryProgress(input: $input) { _docID } }",
            &serde_json::json!({"input": {"progress_key": key, "client_did": owner, "progress": progress}}),
        ).await?,
        Some(row) => txn.execute_with_variables(&format!(
            r#"mutation($input: FileInventoryProgressMutationInputArg!) {{ update_FileInventoryProgress(filter: {{ _docID: {{ _eq: "{}" }}, progress_key: {{ _eq: "{}" }} }}, input: $input) {{ _docID }} }}"#,
            escape_graphql_string(&row.document_id), escape_graphql_string(key)),
            &serde_json::json!({"input": {"progress": progress}}),
        ).await?,
    };
    let operation = if prior.is_some() {
        "update_FileInventoryProgress"
    } else {
        "create_FileInventoryProgress"
    };
    ensure!(
        crate::graphql::mutation_documents(&result, operation)?.is_some_and(|rows| {
            rows.len() == 1
                && rows[0]["_docID"]
                    .as_str()
                    .is_some_and(|id| prior.is_none_or(|prior| prior.document_id == id))
        }),
        "file inventory client progress was not saved"
    );
    Ok(())
}
