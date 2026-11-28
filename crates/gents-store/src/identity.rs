use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use defra_core::signing::{SigningConfig, SigningKeyType};
use identity::{FullIdentity as _, Identity as _, RawIdentity};

pub mod agent;
mod application_policy;
pub mod client_access;
mod enrollment_approval;
mod enrollment_gate;
mod enrollment_grant;
mod error;
mod file_key;
pub mod grant_sets;
pub mod home_status;
pub(crate) mod macos;
pub(crate) mod provision_client;
mod registered;
pub mod request_receipt;

pub use agent::{
    derive_agent_identity, observe_agent_admission, observe_document_permissions,
    provision_agent_identity, provision_agent_identity_authorized,
    provision_agent_identity_for_agent, resolve_agent_identity, AgentAdmissionObservationError,
    AgentProvisionError, ObservedDocumentPermissions, RuntimeAgentProof,
};
pub use application_policy::{
    ApplicationPolicy, PolicyParent, PolicyPermission, PolicyRefusal, PolicyRelation,
    PolicyResource,
};
pub use enrollment_approval::{
    approve_enrollment, grant_as_approver, verify_pending_client_request, ApprovalStep,
    EnrollmentApproval, EnrollmentApprovalError,
};
pub use enrollment_gate::enrollment_decision_gate;
pub use enrollment_grant::{
    administration_owed, after_removal, grant_enrollment_authority, lapse_clock,
    load_enrollment_grant_receipts, membership_lapsed, owed_removals, permission_grant_admitted,
    plan_grant, plan_membership_withdrawal, plan_owing, plan_withdrawal, restoration_step,
    settle_owed_withdrawals, settlement_step, AfterRemoval, EnrollmentGrantError,
    EnrollmentGrantOutcome, EnrollmentGrantReceipt, EnrollmentGrantTuple,
    EnrollmentGrantWithdrawal, RestorationStep, SettlementStep,
};
pub(crate) use enrollment_grant::{
    close_owed_removal, owed_removal_receipts, NODE_ADMINISTRATOR_RELATION,
};
#[cfg(test)]
pub(crate) use enrollment_grant::{NativeTuples, TupleChanges};
pub use error::{identity_key_unavailable, IdentityError, IdentityKeyUnavailable};
pub use file_key::{load_file_identity, load_or_create_file_identity, InsecureKeyPermissions};
pub(crate) use file_key::{publish_private_key_file, read_private_key_file};
pub use grant_sets::{
    node_runtime_permissions, GrantSet, GrantSetError, Recipient, RecipientGrants,
};
pub use macos::{
    load_macos_keychain_identity, load_macos_secure_enclave_identity,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity,
};
pub use provision_client::{
    observe_admission as observe_agent_admission_remote,
    provision as provision_agent_identity_remote, RemoteAgentProvisionError,
};
use registered::raw_identity_from_signing_config;
pub use registered::RegisteredIdentity;

type Result<T, E = IdentityError> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccount {
    pub host_id: String,
    pub deployment_id: String,
}

#[derive(Clone)]
pub struct RuntimeNode {
    pub node_did: String,
    pub identity: Arc<dyn NodeIdentity>,
    pub default_agent_did: String,
    pub display_name: Option<String>,
    pub enabled: bool,
}

impl std::fmt::Debug for RuntimeNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeNode")
            .field("node_did", &self.node_did)
            .field("default_agent_did", &self.default_agent_did)
            .field("display_name", &self.display_name)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait NodeIdentity: Send + Sync {
    fn did(&self) -> &str;

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>>;

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool>;

    fn service_account(&self) -> Option<&ServiceAccount>;

    /// A capability exists only when this identity retains a supported local key.
    fn client_signing_capability(
        &self,
    ) -> std::result::Result<
        crate::config_client::ClientSigningCapability,
        crate::config_client::ConfigClientError,
    > {
        Err(crate::config_client::ConfigClientError::ClientAgentSignerRequired)
    }

    /// The derivation root is available only from an identity that holds its key.
    fn agent_derivation_root(&self) -> Option<AgentDerivationRoot> {
        None
    }

    /// A validated runtime-owned Agent identity carries its own block signer.
    fn managed_agent_signer(&self) -> Option<ManagedAgentSigner> {
        None
    }
}

/// Opaque authority to derive the Agents owned by a held runtime root key.
#[derive(Clone)]
pub struct AgentDerivationRoot {
    identity: Arc<RawIdentity>,
}

/// Signing authority derived and validated for a runtime-owned Agent binding.
#[derive(Clone)]
pub struct ManagedAgentSigner {
    node_did: String,
    /// The proof of the runtime key this signer was derived from, so a local
    /// access the host builds for the Agent carries it (`hostAccess`).
    runtime_proof: Option<agent::RuntimeAgentProof>,
    signer: defra_node::EmbeddedSigner,
    identity: Arc<KeyIdentity>,
}

#[async_trait]
impl NodeIdentity for ManagedAgentSigner {
    fn did(&self) -> &str {
        self.identity.did()
    }
    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        self.identity.sign(payload).await
    }
    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
        self.identity.verify(did, payload, signature).await
    }
    fn service_account(&self) -> Option<&ServiceAccount> {
        self.identity.service_account()
    }
    fn client_signing_capability(
        &self,
    ) -> std::result::Result<
        crate::config_client::ClientSigningCapability,
        crate::config_client::ConfigClientError,
    > {
        self.identity.client_signing_capability()
    }
    fn managed_agent_signer(&self) -> Option<ManagedAgentSigner> {
        Some(self.clone())
    }
}

impl ManagedAgentSigner {
    pub fn did(&self) -> &str {
        self.signer.did()
    }

    pub fn node_did(&self) -> &str {
        &self.node_did
    }

    pub(crate) fn runtime_proof(&self) -> Option<&agent::RuntimeAgentProof> {
        self.runtime_proof.as_ref()
    }

    /// `request`, signed with this Agent's key and decided by document ACP
    /// even where node access control grants the Agent `bypass-dac`.
    pub fn attach(&self, request: query::QueryRequest) -> query::QueryRequest {
        Self::deny_dac_bypass(self.signer.attach(request))
    }

    /// `request` made as a managed Agent, decided by document ACP even where
    /// node access control grants the Agent `bypass-dac`: managed-Agent
    /// signing never resolves the bypass (Lean
    /// `Configuration.toolSessionReadAllowed`).
    pub fn deny_dac_bypass(mut request: query::QueryRequest) -> query::QueryRequest {
        request.context = request.context.with_dac_bypass_denied();
        request
    }
}

impl std::fmt::Debug for ManagedAgentSigner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ManagedAgentSigner")
            .field(&self.did())
            .finish()
    }
}

#[derive(Debug, Clone)]
struct KnownPublicKey {
    key_type: crypto::KeyType,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct KeyIdentity {
    did: String,
    service_account: Option<ServiceAccount>,
    identity: Arc<RawIdentity>,
}

impl KeyIdentity {
    /// Generates an ephemeral Ed25519 identity in memory and registers it with
    /// DefraDB without writing a key file to disk.
    pub fn generate() -> Result<Self> {
        use crypto::Key as _;
        let private_key = crypto::generate_ed25519()?;
        let raw = RawIdentity::from_bytes(crypto::KeyType::Ed25519, private_key.raw())
            .map_err(IdentityError::LoadNodeSigning)?;
        Self::from_raw_identity(raw, None)
    }

    pub fn load_or_create(
        key_path: impl Into<PathBuf>,
        service_account: Option<ServiceAccount>,
    ) -> Result<Self> {
        Self::from_raw_identity(
            load_or_create_file_identity(&key_path.into())?,
            service_account,
        )
    }

    /// Opens only an existing key; initialized homes must not mint a
    /// replacement when their recorded key disappears.
    pub fn load_existing(
        key_path: impl Into<PathBuf>,
        service_account: Option<ServiceAccount>,
    ) -> Result<Self> {
        Self::from_raw_identity(load_file_identity(&key_path.into())?, service_account)
    }

    /// Derive an explicit client signing capability from this held identity.
    pub fn client_signing_capability(
        &self,
    ) -> std::result::Result<
        crate::config_client::ClientSigningCapability,
        crate::config_client::ConfigClientError,
    > {
        let key = zeroize::Zeroizing::new(self.identity.private_key_bytes());
        crate::config_client::ClientSigningCapability::from_ed25519_key(&key)
    }

    fn from_raw_identity(
        identity: RawIdentity,
        service_account: Option<ServiceAccount>,
    ) -> Result<Self> {
        let identity = Arc::new(identity);
        let did = identity.did()?.to_string();
        let public_key_bytes = identity.public_key_bytes();
        register_ed25519_signing_identity(&did, &identity.private_key_bytes(), &public_key_bytes)?;
        Ok(Self {
            did,
            service_account,
            identity,
        })
    }
}

pub fn register_ed25519_signing_identity(
    did: &str,
    private_key_bytes: &[u8],
    public_key_bytes: &[u8],
) -> Result<()> {
    let identity = RawIdentity::from_bytes(crypto::KeyType::Ed25519, private_key_bytes)
        .map_err(IdentityError::LoadNodeSigning)?;
    let derived_did = identity.did()?.to_string();
    if derived_did != did {
        return Err(IdentityError::NodeDidMismatch {
            did: did.to_owned(),
            derived_did,
        });
    }
    if identity.public_key_bytes() != public_key_bytes {
        return Err(IdentityError::NodePublicKeyMismatch {
            did: did.to_owned(),
        });
    }

    defra_core::signing::store_identity(
        did,
        SigningConfig {
            key_type: SigningKeyType::Ed25519,
            private_key_bytes: SigningConfig::private_key_bytes_from_vec(
                private_key_bytes.to_vec(),
            ),
            public_key_bytes: public_key_bytes.to_vec(),
            public_key_hex: lowercase_hex(public_key_bytes),
            remote_signer: None,
            signing_authorization: None,
        },
    )?;
    Ok(())
}

/// Identity string DefraDB stores in commit signature headers for a
/// `did:key` principal: the hexadecimal public key, not the DID.
pub fn commit_signer_identity_for_did(did: &str) -> Result<String> {
    let (_, public_key_bytes) =
        crypto::parse_did_key(did).map_err(|source| IdentityError::ParseCommitSignerDid {
            did: did.to_owned(),
            source,
        })?;
    Ok(lowercase_hex(&public_key_bytes))
}

/// The DID that signed a commit, from `_commits { signature { identity type } }`:
/// the inverse of [`commit_signer_identity_for_did`]. DefraDB verifies the
/// signature when the block arrives, so this names the node that wrote it.
/// The DID is derived as DefraDB derives it at merge
/// (`verified_signature_signer_did`), so a compressed secp256k1 or P-256
/// header key names the same DID as its uncompressed form.
pub fn commit_signer_did(identity: &str, signature_type: &str) -> Result<String> {
    let parsed =
        serde_json::from_value(serde_json::Value::from(signature_type)).map_err(|source| {
            IdentityError::UnknownCommitSignatureType {
                signature_type: signature_type.to_owned(),
                source,
            }
        })?;
    let key_type = match parsed {
        defra_core::SignatureType::EdDSA => crypto::KeyType::Ed25519,
        defra_core::SignatureType::ES256K => crypto::KeyType::Secp256k1,
        defra_core::SignatureType::ES256 => crypto::KeyType::Secp256r1,
        defra_core::SignatureType::BLSAugV1 => crypto::KeyType::Bls12381,
    };
    crypto::public_key_from_string(key_type, identity)
        .and_then(|key| key.did())
        .map_err(|source| IdentityError::UnreadableCommitSigner {
            identity: identity.to_owned(),
            signature_type: signature_type.to_owned(),
            source,
        })
}

/// DefraDB tolerates up to 60s of clock skew and its bearers carry no nonce,
/// so a replayed bearer stays usable for at most two minutes.
pub(crate) const DEFRADB_BEARER_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// Mint an `Authorization` header authenticating one DefraDB HTTP request as
/// `did`. DefraDB verifies the JWT against the request's `Host` header, so
/// `audience` must be the `host[:port]` the request is sent to. A DID whose
/// private key bytes this process did not register is refused, not silently
/// downgraded to an anonymous request.
pub fn defradb_bearer_authorization(did: &str, audience: &str) -> Result<String> {
    let config = defra_core::signing::get_identity(did).ok_or_else(|| {
        IdentityError::SigningIdentityNotLoaded {
            did: did.to_owned(),
        }
    })?;
    defradb_bearer_authorization_with_config(did, audience, &config)
}

pub(crate) fn defradb_bearer_authorization_with_config(
    did: &str,
    audience: &str,
    config: &SigningConfig,
) -> Result<String> {
    mint_defradb_bearer(did, audience, config, DEFRADB_BEARER_LIFETIME)
}

/// A bearer of `lifetime` minted now. A subscription handshake mints its own:
/// its renewal, half a lifetime after its arm, then falls before the runtime
/// ends the stream at the bearer's expiry (Lean
/// `SubscriptionStream.a_fresh_bearer_renews_before_the_cut`). An endpoint
/// reuses one for its other requests ([`crate::config_client::GraphqlEndpoint`]).
pub(crate) fn mint_defradb_bearer(
    did: &str,
    audience: &str,
    config: &SigningConfig,
    lifetime: Duration,
) -> Result<String> {
    let identity = raw_identity_from_signing_config(config).map_err(|source| {
        IdentityError::LoadSigningIdentity {
            did: did.to_owned(),
            source: Box::new(source),
        }
    })?;
    let derived_did = identity.did()?.to_string();
    if derived_did != did {
        return Err(IdentityError::NodeDidMismatch {
            did: did.to_owned(),
            derived_did,
        });
    }
    let token = identity::new_token(&identity, lifetime, Some(audience.to_string()), None)
        .map_err(|source| IdentityError::MintBearer {
            did: did.to_owned(),
            source,
        })?;
    let token = String::from_utf8(token).map_err(IdentityError::BearerNotUtf8)?;
    Ok(format!("Bearer {token}"))
}

pub fn can_mint_defradb_bearer(did: &str) -> bool {
    defra_core::signing::get_identity(did).is_some_and(|config| config.has_local_private_key())
}

impl KeyIdentity {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn sign_now(&self, payload: &[u8]) -> Result<Vec<u8>> {
        self.identity
            .sign(payload)
            .map_err(|source| IdentityError::SignPayload {
                did: self.did.clone(),
                source,
            })
    }
}

#[async_trait]
impl NodeIdentity for KeyIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    fn client_signing_capability(
        &self,
    ) -> std::result::Result<
        crate::config_client::ClientSigningCapability,
        crate::config_client::ConfigClientError,
    > {
        KeyIdentity::client_signing_capability(self)
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        Ok(self
            .identity
            .sign(payload)
            .map_err(|source| IdentityError::SignPayload {
                did: self.did.clone(),
                source,
            })?)
    }

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
        let public_key = if did == self.did {
            KnownPublicKey {
                key_type: self.identity.key_type(),
                bytes: self.identity.public_key_bytes(),
            }
        } else {
            known_public_key_for_did(did)?
        };

        Ok(verify_with_public_key(did, public_key, payload, signature)?)
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        self.service_account.as_ref()
    }

    fn agent_derivation_root(&self) -> Option<AgentDerivationRoot> {
        Some(AgentDerivationRoot {
            identity: self.identity.clone(),
        })
    }
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// A `did:key` names its public key; every identity this runtime holds or
/// registers is one, since each is checked against the DID its key derives.
fn known_public_key_for_did(did: &str) -> Result<KnownPublicKey> {
    if did.starts_with("did:key:") {
        let (key_type, bytes) =
            crypto::parse_did_key(did).map_err(|source| IdentityError::ParseDidKey {
                did: did.to_owned(),
                source,
            })?;
        return Ok(KnownPublicKey { key_type, bytes });
    }

    Err(IdentityError::NoPublicKey {
        did: did.to_owned(),
    })
}

fn verify_with_public_key(
    did: &str,
    public_key: KnownPublicKey,
    payload: &[u8],
    signature: &[u8],
) -> Result<bool> {
    let public_key = crypto::public_key_from_bytes(public_key.key_type, &public_key.bytes)?;
    public_key
        .verify(payload, signature)
        .map_err(|source| IdentityError::VerifyPayload {
            did: did.to_owned(),
            source,
        })
}

pub fn verify_did_signature(did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
    verify_with_public_key(did, known_public_key_for_did(did)?, payload, signature)
}

#[cfg(test)]
mod tests;
