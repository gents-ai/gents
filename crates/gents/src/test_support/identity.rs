use std::sync::Arc;

use async_trait::async_trait;
use gents_store::identity::{IdentityError, NodeIdentity, ServiceAccount};

pub(crate) struct UnmanagedSigner(pub(crate) Arc<dyn NodeIdentity>);

#[async_trait]
impl NodeIdentity for UnmanagedSigner {
    fn did(&self) -> &str {
        self.0.did()
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, IdentityError> {
        self.0.sign(payload).await
    }

    async fn verify(
        &self,
        did: &str,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<bool, IdentityError> {
        self.0.verify(did, payload, signature).await
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        self.0.service_account()
    }
}
