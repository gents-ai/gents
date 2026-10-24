use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::file_image_hydration::{
    FileImageDemand, FileImageHydrationReceipt, FileImageSelection,
};

use crate::config_client::ConfigAccess;
use crate::NodeIdentity;

/// A selected receipt delegates a local read of its exact observation to the
/// named requester. It grants neither source-database access nor write access.
pub struct VerifiedFileImageReceipt(FileImageHydrationReceipt);

impl VerifiedFileImageReceipt {
    pub async fn verify(
        receipt: FileImageHydrationReceipt,
        demand: &FileImageDemand,
        verifier: &dyn NodeIdentity,
    ) -> Result<Self> {
        receipt.validate_shape()?;
        anyhow::ensure!(
            &receipt.demand == demand,
            "file review receipt crossed its demand"
        );
        anyhow::ensure!(
            verifier
                .verify(
                    &demand.admission.node_did,
                    &receipt.signing_payload()?,
                    &receipt.signature
                )
                .await?,
            "file review receipt signature is invalid"
        );
        Ok(Self(receipt))
    }

    pub fn selection(&self) -> &FileImageSelection {
        &self.0.selection
    }

    pub fn detail(&self) -> &str {
        &self.0.detail
    }

    /// The creator remains the DefraDB owner on a replica. Install the receipt's
    /// reader grant only after that signed creator registration has committed.
    pub async fn grant_local_read(&self, node: Arc<EmbeddedNode>) -> Result<bool> {
        let FileImageSelection::Selected { document } = &self.0.selection else {
            return Ok(false);
        };
        let scope = &self.0.demand.admission;
        grant_source_file_read(
            node,
            gents_protocol::schemas::TOOL_FILE_CHANGE_NAME,
            &scope.node_did,
            &scope.requester_did,
            document,
        )
        .await
    }
}

pub(crate) async fn grant_source_file_read(
    node: Arc<EmbeddedNode>,
    collection_name: &'static str,
    source_did: &str,
    requester_did: &str,
    document: &str,
) -> Result<bool> {
    anyhow::ensure!(
        matches!(
            collection_name,
            gents_protocol::schemas::TOOL_FILE_CHANGE_NAME
                | gents_protocol::schemas::FILE_INVENTORY_PAGE_NAME
        ),
        "unsupported file-read collection"
    );
    let document = document.to_owned();
    let source_did = source_did.to_owned();
    let requester_did = requester_did.to_owned();
    anyhow::ensure!(
        node.node_identity_did() == Some(requester_did.as_str()),
        "file review receipt does not authorize this client"
    );
    ConfigAccess::local(node)
        .run_local_owned(move |node| async move {
            let collection = node
                .get_collection(collection_name)?
                .context("local file-image collection is unavailable")?;
            let policy = collection
                .policy
                .context("file-image storage has no document policy")?;
            anyhow::ensure!(
                policy.id == gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID
                    && policy.resource_name
                        == gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                "file-image storage has an unexpected document policy"
            );
            let acp = node.document_acp();
            let Some(owner) = acp
                .get_doc_owner(&policy.id, &policy.resource_name, &document)
                .await?
            else {
                return Ok(false);
            };
            anyhow::ensure!(
                owner.as_str() == source_did,
                "file review receipt signer does not own the retained observation"
            );
            let reader = defra_node::AcpIdentity::Authenticated(requester_did.parse()?);
            if acp
                .check_doc_access(
                    &reader,
                    defra_node::DocumentPermission::Read,
                    &policy.id,
                    &policy.resource_name,
                    &document,
                )
                .await?
            {
                return Ok(true);
            }
            node.add_dac_actor_relationship(
                &source_did,
                collection_name,
                &document,
                "reader",
                &requester_did,
            )
            .await?;
            Ok(true)
        })
        .await
}

#[cfg(test)]
mod tests;
