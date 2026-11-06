use std::sync::Arc;

use gents_protocol::agent_identity::AgentIdentityBinding;
use identity::{Identity as _, RawIdentity};
use zeroize::Zeroizing;

use super::{IdentityError, KeyIdentity, ManagedAgentSigner, NodeIdentity, Result, ServiceAccount};

/// The creation nonce is public, immutable, and independent of labels and
/// document hashes. Its random allocation prevents a recreated label from
/// inheriting the previous Agent's authority. Hardware-backed roots without
/// exportable key material cannot use this derivation contract.
pub fn provision_agent_identity(node: &dyn NodeIdentity) -> Result<AgentIdentityBinding> {
    let nonce = rand::random::<[u8; 32]>();
    let creation_nonce = super::lowercase_hex(&nonce);
    let identity = derive_agent_identity(node, &creation_nonce)?;
    Ok(AgentIdentityBinding {
        creation_nonce,
        agent_did: identity.did().to_owned(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum AgentProvisionError {
    #[error("runtime identity does not match the local node")]
    NodeMismatch,
    #[error("the selected agent signer is missing, mismatched, or not locally provisioned")]
    InvalidAgentSigner,
    #[error("the caller cannot administer this node")]
    Unauthorized,
    #[error("node authorization is unavailable")]
    AuthorizationUnavailable,
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Access(#[from] crate::store::config_client::ConfigClientError),
}

/// Allocate and provision a fresh agent only after native authorization of the
/// caller. NAC grants are detached from the authorization snapshot: a
/// concurrent revocation cannot fence a grant already in flight. A failed
/// grant discards the fresh binding, including when its result is ambiguous.
pub async fn provision_agent_identity_authorized(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    caller: &identity::Did,
) -> std::result::Result<AgentIdentityBinding, AgentProvisionError> {
    use crate::store::config_client::{ConfigAccess, NodePermission};
    if node.node_identity_did() != Some(runtime.did()) {
        return Err(AgentProvisionError::NodeMismatch);
    }
    let principal = identity::Did::new(runtime.did()).map_err(IdentityError::from)?;
    let access = ConfigAccess::local_as(node.clone(), principal);
    let node_did = runtime.did();
    let decision = access
        .transact_readonly("agent.provision_authorization", |txn| {
            Box::pin(async move {
                let query = format!(
                    r#"{{ Node(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
                    crate::store::graphql::escape_graphql_string(node_did)
                );
                let response = txn.execute(&query).await?;
                let Some([row]) = response["data"]["Node"].as_array().map(Vec::as_slice) else {
                    return Ok::<_, crate::store::config_client::ConfigClientError>(None);
                };
                let Some(doc_id) = row["_docID"].as_str() else {
                    return Ok(None);
                };
                let result = txn
                    .observe_document_access(
                        caller,
                        "Node",
                        &[doc_id.to_owned()],
                        defra_node::DocumentAccessMode::Named {
                            permission: "update".to_owned(),
                            node_permission: NodePermission::DocumentUpdate,
                        },
                    )
                    .await?;
                Ok(result.into_iter().next())
            })
        })
        .await?;
    match decision {
        Some(defra_node::DocumentAccessDecision::Allowed) => {}
        Some(defra_node::DocumentAccessDecision::Denied) => {
            return Err(AgentProvisionError::Unauthorized)
        }
        _ => return Err(AgentProvisionError::AuthorizationUnavailable),
    }
    grant_fresh_agent(node, runtime).await
}

async fn grant_fresh_agent(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
) -> std::result::Result<AgentIdentityBinding, AgentProvisionError> {
    let binding = provision_agent_identity(runtime)?;
    grant_agent_node_capabilities(node, runtime, &binding).await?;
    Ok(binding)
}

pub(crate) async fn grant_agent_node_capabilities(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    binding: &AgentIdentityBinding,
) -> std::result::Result<(), AgentProvisionError> {
    use crate::store::config_client::{ConfigAccess, NodePermission};
    if node.node_identity_did() != Some(runtime.did()) {
        return Err(AgentProvisionError::NodeMismatch);
    }
    resolve_agent_identity(runtime, binding)?;
    let principal = identity::Did::new(runtime.did()).map_err(IdentityError::from)?;
    let access = ConfigAccess::local_as(node.clone(), principal);
    let agent = identity::Did::new(&binding.agent_did).map_err(IdentityError::from)?;
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        access.grant_node_permission(&agent, permission).await?;
    }
    Ok(())
}

/// Allocate a binding for an explicitly held executing Agent signer. Native
/// creation and relationship operations remain the authorization boundary.
/// The new Agent receives its node capabilities only after publication.
pub async fn provision_agent_identity_for_agent(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    actor: &dyn NodeIdentity,
    selected_agent_did: &str,
) -> std::result::Result<AgentIdentityBinding, AgentProvisionError> {
    use crate::store::config_client::ConfigAccess;
    if node.node_identity_did() != Some(runtime.did()) {
        return Err(AgentProvisionError::NodeMismatch);
    }
    if actor.did() != selected_agent_did || actor.did() == runtime.did() {
        return Err(AgentProvisionError::InvalidAgentSigner);
    }
    let caller = identity::Did::new(actor.did()).map_err(IdentityError::from)?;
    let access = ConfigAccess::local_managed_agent(node.clone(), caller, actor)
        .map_err(|_| AgentProvisionError::InvalidAgentSigner)?;
    let binding = access.transact_readonly("agent.creator_binding", |txn| {
        Box::pin(async move {
            let query = format!(r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 2) {{ agent_did creation_nonce }} }}"#,
                crate::store::graphql::escape_graphql_string(selected_agent_did));
            let response = txn.execute(&query).await?;
            let binding = response["data"]["Agent"].as_array().and_then(|rows| match rows.as_slice() {
                [row] => Some(AgentIdentityBinding {
                    agent_did: row["agent_did"].as_str()?.to_owned(),
                    creation_nonce: row["creation_nonce"].as_str()?.to_owned(),
                }),
                _ => None,
            });
            Ok::<_, crate::store::config_client::ConfigClientError>(binding)
        })
    }).await?.ok_or(AgentProvisionError::InvalidAgentSigner)?;
    let expected = resolve_agent_identity(runtime, &binding)
        .map_err(|_| AgentProvisionError::InvalidAgentSigner)?;
    let challenge = b"gents/agent-provision/selected-signer/v1";
    let signature = actor
        .sign(challenge)
        .await
        .map_err(|_| AgentProvisionError::InvalidAgentSigner)?;
    if !expected
        .verify(actor.did(), challenge, &signature)
        .await
        .map_err(|_| AgentProvisionError::InvalidAgentSigner)?
    {
        return Err(AgentProvisionError::InvalidAgentSigner);
    }
    provision_agent_identity(runtime).map_err(Into::into)
}

pub fn resolve_agent_identity(
    node: &dyn NodeIdentity,
    binding: &AgentIdentityBinding,
) -> Result<Arc<dyn NodeIdentity>> {
    let identity = derive_agent_key(node, &binding.creation_nonce)?;
    if identity.did() != binding.agent_did {
        return Err(IdentityError::AgentIdentityMismatch {
            agent_did: binding.agent_did.clone(),
            derived_did: identity.did().to_owned(),
        });
    }
    let raw = &identity.identity;
    let public = raw.public_key_bytes();
    let config = defra_core::signing::SigningConfig {
        key_type: defra_core::signing::SigningKeyType::Ed25519,
        private_key_bytes: defra_core::signing::SigningConfig::private_key_bytes_from_vec(
            raw.private_key_bytes(),
        ),
        public_key_hex: super::lowercase_hex(&public),
        public_key_bytes: public,
        remote_signer: None,
        signing_authorization: None,
    };
    let signer =
        defra_node::EmbeddedSigner::from_config(config).map_err(IdentityError::AgentBlockSigner)?;
    if signer.did() != identity.did() {
        return Err(IdentityError::AgentIdentityMismatch {
            agent_did: binding.agent_did.clone(),
            derived_did: signer.did().to_owned(),
        });
    }
    let identity = Arc::new(identity);
    Ok(Arc::new(ManagedAgentIdentity {
        identity: identity.clone(),
        signer: ManagedAgentSigner {
            node_did: node.did().to_owned(),
            signer,
            identity,
        },
    }))
}

struct ManagedAgentIdentity {
    identity: Arc<KeyIdentity>,
    signer: ManagedAgentSigner,
}

#[async_trait::async_trait]
impl NodeIdentity for ManagedAgentIdentity {
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
        crate::store::config_client::ClientSigningCapability,
        crate::store::config_client::ConfigClientError,
    > {
        self.identity.client_signing_capability()
    }
    fn managed_agent_signer(&self) -> Option<ManagedAgentSigner> {
        Some(self.signer.clone())
    }
}

/// HKDF's versioned salt and length-framed node DID / canonical nonce bind the
/// derived signing seed to this runtime and creation, independent of labels.
pub fn derive_agent_identity(
    node: &dyn NodeIdentity,
    creation_nonce: &str,
) -> Result<Arc<dyn NodeIdentity>> {
    Ok(Arc::new(derive_agent_key(node, creation_nonce)?))
}

fn derive_agent_key(node: &dyn NodeIdentity, creation_nonce: &str) -> Result<KeyIdentity> {
    if creation_nonce.len() != 64
        || !creation_nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(IdentityError::InvalidAgentCreationNonce);
    }
    let held =
        node.agent_derivation_root()
            .ok_or_else(|| IdentityError::AgentDerivationUnavailable {
                node_did: node.did().to_owned(),
            })?;
    let root = &held.identity;
    let derived_did = root.did()?.to_string();
    if derived_did != node.did() {
        return Err(IdentityError::RegisteredDidMismatch {
            did: node.did().to_owned(),
            derived_did,
        });
    }
    let root_bytes = Zeroizing::new(root.private_key_bytes());
    let mut context = Vec::with_capacity(16 + node.did().len() + creation_nonce.len());
    for component in [node.did(), creation_nonce] {
        context.extend_from_slice(&(component.len() as u64).to_be_bytes());
        context.extend_from_slice(component.as_bytes());
    }
    let hkdf =
        hkdf::Hkdf::<sha2::Sha256>::new(Some(b"gents/agent-identity/v2"), root_bytes.as_slice());
    let mut seed = Zeroizing::new([0_u8; 32]);
    hkdf.expand(&context, seed.as_mut())
        .map_err(|_| IdentityError::AgentDerivationLength)?;
    let keypair = Zeroizing::new(crypto::ed25519_key_from_seed(seed.as_ref())?);
    let identity = RawIdentity::from_bytes(crypto::KeyType::Ed25519, keypair.as_slice())?;
    KeyIdentity::from_raw_identity(identity, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provisioned_identity_reopens_without_reusing_a_creation() {
        let directory = tempfile::tempdir().unwrap();
        let key = directory.path().join("node.key");
        let root = KeyIdentity::load_or_create(&key, None).unwrap();
        let first_binding = provision_agent_identity(&root).unwrap();
        let second_binding = provision_agent_identity(&root).unwrap();
        let first = resolve_agent_identity(&root, &first_binding).unwrap();
        let second = resolve_agent_identity(&root, &second_binding).unwrap();
        let reloaded = KeyIdentity::load_existing(&key, None).unwrap();
        let reopened = resolve_agent_identity(&reloaded, &first_binding).unwrap();
        let other_root =
            KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
        let other = derive_agent_identity(&other_root, &first_binding.creation_nonce).unwrap();
        assert_eq!(first.did(), reopened.did());
        assert_ne!(first.did(), root.did());
        assert_ne!(first.did(), second.did());
        assert_ne!(first.did(), other.did());
        let registered =
            super::super::RegisteredIdentity::from_registered_did(first.did(), None).unwrap();
        let signature = registered.sign(b"agent-owned operation").await.unwrap();
        assert!(reopened
            .verify(first.did(), b"agent-owned operation", &signature)
            .await
            .unwrap());
        assert!(second
            .verify(second.did(), b"agent-owned operation", &signature)
            .await
            .is_err());
        assert!(matches!(
            resolve_agent_identity(&other_root, &first_binding),
            Err(IdentityError::AgentIdentityMismatch { .. })
        ));
    }

    #[test]
    fn derived_identity_matches_lean_cases() {
        use rapidhash::{HashMapExt, RapidHashMap};
        let directory = tempfile::tempdir().unwrap();
        let mut roots = RapidHashMap::new();
        for case in crate::lean_vocab_test::lean_identity_derivation_cases() {
            let mut derive = |input: &crate::lean_vocab_test::LeanIdentityDerivationInput| {
                let next = roots.len();
                let root = roots.entry(input.node_key.clone()).or_insert_with(|| {
                    KeyIdentity::load_or_create(
                        directory.path().join(format!("node-{next}.key")),
                        None,
                    )
                    .unwrap()
                });
                derive_agent_identity(root, &input.creation_nonce).unwrap()
            };
            let left = derive(&case.left);
            let right = derive(&case.right);
            assert_eq!(
                left.did() == right.did(),
                case.expected_same_did,
                "{}: {:?} -> {:?}",
                case.name,
                case.left.label,
                case.right.label
            );
        }
    }

    #[test]
    fn invalid_nonce_and_nonexportable_root_fail_without_node_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let root = KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
        for nonce in ["", "agent-label", &"A".repeat(64), &"0".repeat(63)] {
            assert!(matches!(
                derive_agent_identity(&root, nonce),
                Err(IdentityError::InvalidAgentCreationNonce)
            ));
        }
        let binding = provision_agent_identity(&root).unwrap();
        let registered =
            super::super::RegisteredIdentity::from_registered_did(root.did(), None).unwrap();
        assert!(matches!(
            resolve_agent_identity(&registered, &binding),
            Err(IdentityError::AgentDerivationUnavailable { .. })
        ));
    }
}

#[cfg(test)]
#[path = "agent_provisioning_tests.rs"]
mod provisioning_tests;
