use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha512};

use gents::identity::IdentityError;
use gents::{NodeIdentity, ServiceAccount};

#[allow(dead_code)]
pub(crate) struct StubNodeIdentity {
    pub did: String,
}

impl StubNodeIdentity {
    #[allow(dead_code)]
    pub(crate) fn new(did: impl Into<String>) -> Self {
        Self { did: did.into() }
    }

    #[allow(dead_code)]
    pub(crate) fn arc(did: impl Into<String>) -> Arc<dyn NodeIdentity> {
        Arc::new(Self::new(did))
    }
}

#[async_trait]
impl NodeIdentity for StubNodeIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, _payload: &[u8]) -> Result<Vec<u8>, IdentityError> {
        panic!(
            "StubNodeIdentity::sign called for {} — routing tests must not sign",
            self.did
        )
    }

    async fn verify(
        &self,
        _did: &str,
        _payload: &[u8],
        _sig: &[u8],
    ) -> Result<bool, IdentityError> {
        panic!(
            "StubNodeIdentity::verify called for {} — routing tests must not verify",
            self.did
        )
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }
}

#[allow(dead_code)]
pub(crate) struct SigningStubNodeIdentity {
    did: String,
}

#[allow(dead_code)]
impl SigningStubNodeIdentity {
    #[allow(dead_code)]
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
