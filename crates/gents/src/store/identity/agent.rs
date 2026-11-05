use std::sync::Arc;

use gents_protocol::agent_identity::AgentIdentityBinding;
use identity::{Identity as _, RawIdentity};
use zeroize::Zeroizing;

use super::{IdentityError, KeyIdentity, NodeIdentity, Result};

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
    let binding = provision_agent_identity(runtime)?;
    let agent = identity::Did::new(&binding.agent_did).map_err(IdentityError::from)?;
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DacRelationAdd,
    ] {
        access.grant_node_permission(&agent, permission).await?;
    }
    Ok(binding)
}

pub fn resolve_agent_identity(
    node: &dyn NodeIdentity,
    binding: &AgentIdentityBinding,
) -> Result<Arc<dyn NodeIdentity>> {
    let identity = derive_agent_identity(node, &binding.creation_nonce)?;
    if identity.did() != binding.agent_did {
        return Err(IdentityError::AgentIdentityMismatch {
            agent_did: binding.agent_did.clone(),
            derived_did: identity.did().to_owned(),
        });
    }
    Ok(identity)
}

/// HKDF's versioned salt and length-framed node DID / canonical nonce bind the
/// derived signing seed to this runtime and creation, independent of labels.
pub fn derive_agent_identity(
    node: &dyn NodeIdentity,
    creation_nonce: &str,
) -> Result<Arc<dyn NodeIdentity>> {
    if creation_nonce.len() != 64
        || !creation_nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(IdentityError::InvalidAgentCreationNonce);
    }
    let config = defra_core::signing::get_identity(node.did()).ok_or_else(|| {
        IdentityError::NoDefraSigningIdentity {
            did: node.did().to_owned(),
        }
    })?;
    if !config.has_local_private_key() {
        return Err(IdentityError::AgentDerivationUnavailable {
            node_did: node.did().to_owned(),
        });
    }
    let root = super::registered::raw_identity_from_signing_config(&config)?;
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
    Ok(Arc::new(KeyIdentity::from_raw_identity(identity, None)?))
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
        let mut config = defra_core::signing::get_identity(root.did()).unwrap();
        config.private_key_bytes.clear();
        defra_core::signing::store_identity(root.did(), config);
        assert!(matches!(
            resolve_agent_identity(&root, &binding),
            Err(IdentityError::AgentDerivationUnavailable { .. })
        ));
    }
}

#[cfg(test)]
#[path = "agent_provisioning_tests.rs"]
mod provisioning_tests;
