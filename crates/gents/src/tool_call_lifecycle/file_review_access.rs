use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents_protocol::file_image_hydration::{
    FileImageDemand, FileImageHydrationReceipt, FileImageSelection,
};

use crate::config_client::ConfigAccess;
use crate::NodeIdentity;

#[derive(Debug, thiserror::Error)]
pub enum FileReviewAccessError {
    #[error("file review receipt crossed its demand")]
    FileReviewReceiptCrossedDemand,
    #[error("file review receipt signature is invalid")]
    FileReviewReceiptSignatureInvalid,
    #[error("file review receipt does not authorize this client")]
    FileReviewReceiptDoesNotAuthorize,
    #[error("file-image storage has an unexpected document policy")]
    FileImageStorageHasUnexpectedDocument,
    #[error("file review receipt signer does not own the retained observation")]
    FileReviewReceiptSignerDoesNot,
    #[error("local file-image collection is unavailable")]
    LocalFileImageCollectionUnavailable,
    #[error("file-image storage has no document policy")]
    FileImageStorageHasNoDocument,
    #[error(transparent)]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    FileImageHydration(#[from] gents_protocol::file_image_hydration::FileImageHydrationError),
    #[error(transparent)]
    Acp(#[from] acp::error::Error),
    #[error(transparent)]
    Node(#[from] defra_node::Error),
    #[error(transparent)]
    Did(#[from] identity::Error),
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
}

type Result<T, E = FileReviewAccessError> = std::result::Result<T, E>;

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
        if &receipt.demand != demand {
            return Err(FileReviewAccessError::FileReviewReceiptCrossedDemand);
        }
        if !(verifier
            .verify(
                &demand.admission.node_did,
                &receipt.signing_payload()?,
                &receipt.signature,
            )
            .await?)
        {
            return Err(FileReviewAccessError::FileReviewReceiptSignatureInvalid);
        }
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
        let document = document.clone();
        let scope = self.0.demand.admission.clone();
        if node.node_identity_did() != Some(scope.requester_did.as_str()) {
            return Err(FileReviewAccessError::FileReviewReceiptDoesNotAuthorize);
        }
        ConfigAccess::local(node)
            .run_local_owned(move |node| async move {
                let collection = node
                    .get_collection(gents_protocol::schemas::TOOL_FILE_CHANGE_NAME)?
                    .ok_or(FileReviewAccessError::LocalFileImageCollectionUnavailable)?;
                let policy = collection
                    .policy
                    .ok_or(FileReviewAccessError::FileImageStorageHasNoDocument)?;
                if !(policy.id == gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID
                    && policy.resource_name
                        == gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE)
                {
                    return Err(FileReviewAccessError::FileImageStorageHasUnexpectedDocument);
                }
                let acp = node.document_acp();
                let Some(owner) = acp
                    .get_doc_owner(&policy.id, &policy.resource_name, &document)
                    .await?
                else {
                    return Ok(false);
                };
                if owner.as_str() != scope.node_did {
                    return Err(FileReviewAccessError::FileReviewReceiptSignerDoesNot);
                }
                let reader = defra_node::AcpIdentity::Authenticated(scope.requester_did.parse()?);
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
                    &scope.node_did,
                    gents_protocol::schemas::TOOL_FILE_CHANGE_NAME,
                    &document,
                    "reader",
                    &scope.requester_did,
                )
                .await?;
                Ok(true)
            })
            .await
    }
}

#[cfg(test)]
mod tests;
