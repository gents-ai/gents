use std::sync::Arc;

use async_trait::async_trait;
use gents_store::identity::{IdentityError, NodeIdentity, ServiceAccount};
use sha2::{Digest, Sha512};

pub(crate) struct SigningStubNodeIdentity {
    did: String,
}

impl SigningStubNodeIdentity {
    pub(crate) fn arc(did: impl Into<String>) -> Arc<dyn NodeIdentity> {
        Arc::new(Self { did: did.into() })
    }

    fn signature(&self, payload: &[u8]) -> Vec<u8> {
        let mut digest = Sha512::new();
        digest.update(b"gents-test-agent-request-signature-v1\0");
        digest.update(self.did.as_bytes());
        digest.update(b"\0");
        digest.update(payload);
        digest.finalize().to_vec()
    }
}

#[async_trait]
impl NodeIdentity for SigningStubNodeIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, IdentityError> {
        Ok(self.signature(payload))
    }

    async fn verify(
        &self,
        did: &str,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<bool, IdentityError> {
        Ok(did == self.did && signature == self.signature(payload))
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }
}

#[cfg(test)]
pub(crate) struct UnmanagedSigner(pub(crate) Arc<dyn NodeIdentity>);

#[cfg(test)]
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
