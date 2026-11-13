use std::sync::Arc;

use gents_protocol::agent_identity::{AgentAdmissionFacts, AgentIdentityBinding};
use identity::{Identity as _, RawIdentity};
use zeroize::Zeroizing;

use super::AgentDerivationRoot;
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
    #[error("Agent {agent_did:?} exists under more than one node ({}); name the node", nodes.join(", "))]
    AmbiguousAgent {
        agent_did: String,
        nodes: Vec<String>,
    },
    #[error("the caller cannot administer this node")]
    Unauthorized,
    #[error("node authorization is unavailable")]
    AuthorizationUnavailable,
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
}

/// The Node permission that is the agent catalog (Lean
/// `SelfConfig.ActorGrants.catalog`): create, clone and choosing the default.
const MANAGE_AGENTS: &str = "manage_agents";

/// Allocate a fresh agent binding only after the caller is natively allowed
/// the agent catalog (`manage_agents` on the Node). It grants nothing: the Agent's baseline is projected from its
/// document as the publishing actor, in the publishing transaction.
pub async fn provision_agent_identity_authorized(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    caller: &identity::Did,
) -> std::result::Result<AgentIdentityBinding, AgentProvisionError> {
    use crate::config_client::{ConfigAccess, NodePermission};
    if node.node_identity_did() != Some(runtime.did()) {
        return Err(AgentProvisionError::NodeMismatch);
    }
    let principal = identity::Did::new(runtime.did()).map_err(IdentityError::from)?;
    let access = ConfigAccess::local_as(node.clone(), principal);
    let node_did = runtime.did();
    let decision = access
        .transact_readonly("agent.provision_authorization", |txn| {
            Box::pin(async move {
                use gents_protocol::document_query::{
                    fields::node as fields, Filter, Query, Selection,
                };
                let rendered = Query::select(Selection::doc_id())
                    .filter(Filter::eq(fields::NODE_DID, node_did))
                    .limit(2)
                    .render();
                let response = txn.run(&rendered).await?;
                let Some([row]) = response["data"]["Node"].as_array().map(Vec::as_slice) else {
                    return Ok::<_, crate::config_client::ConfigClientError>(None);
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
                            permission: MANAGE_AGENTS.to_owned(),
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
    provision_agent_identity(runtime).map_err(Into::into)
}

#[derive(Debug, thiserror::Error)]
pub enum AgentAdmissionObservationError {
    #[error("the node DID does not name this runtime's node")]
    NodeMismatch,
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
}

/// Observes, as the runtime, the native ACP grants `actor` holds for agent
/// operations (Lean `SelfConfig.ActorGrants`) and the Node's current default
/// Agent as the runtime reads it. It decides nothing: agent admission decides
/// from these facts and DefraDB authorizes every write. A grant counts as held
/// only when DefraDB answers `Allowed`; a denied or unavailable answer, or a
/// document the runtime cannot find, is not held.
pub async fn observe_agent_admission(
    node: &Arc<defra_node::Node>,
    node_did: &str,
    actor: &identity::Did,
    targets: &[String],
) -> std::result::Result<AgentAdmissionFacts, AgentAdmissionObservationError> {
    use crate::config_client::{ConfigAccess, ConfigClientError, NodePermission};
    use gents_protocol::document_query::{
        self as query,
        fields::{
            agent as agent_fields, default_agent_selection as default_fields, node as node_fields,
        },
        Filter, Query, Selection,
    };
    if node.node_identity_did() != Some(node_did) {
        return Err(AgentAdmissionObservationError::NodeMismatch);
    }
    let runtime = identity::Did::new(node_did).map_err(IdentityError::from)?;
    let facts = ConfigAccess::local_as(node.clone(), runtime)
        .transact_readonly("agent.admission_facts", |txn| {
            Box::pin(async move {
                let allowed = |decisions: Vec<defra_node::DocumentAccessDecision>| {
                    decisions
                        .into_iter()
                        .map(|decision| {
                            matches!(decision, defra_node::DocumentAccessDecision::Allowed)
                        })
                        .collect::<Vec<_>>()
                };
                let nodes = txn
                    .run(
                        &Query::select(Selection::doc_id())
                            .filter(Filter::eq(node_fields::NODE_DID, node_did))
                            .limit(2)
                            .render(),
                    )
                    .await?;
                let catalog = match query::rows(&nodes, "Node") {
                    [row] => match row["_docID"].as_str() {
                        Some(doc_id) => allowed(
                            txn.observe_document_access(
                                actor,
                                "Node",
                                &[doc_id.to_owned()],
                                defra_node::DocumentAccessMode::Named {
                                    permission: MANAGE_AGENTS.to_owned(),
                                    node_permission: NodePermission::DocumentUpdate,
                                },
                            )
                            .await?,
                        )
                        .first()
                        .copied()
                        .unwrap_or(false),
                        None => false,
                    },
                    _ => false,
                };
                let mut configures = Vec::new();
                if !targets.is_empty() {
                    let agents = txn
                        .run(
                            &Query::select(Selection::doc_id().field(agent_fields::AGENT_DID))
                                .filter(Filter::eq(agent_fields::NODE_DID, node_did).and(
                                    Filter::any_of(
                                        agent_fields::AGENT_DID,
                                        targets.iter().map(String::as_str),
                                    ),
                                ))
                                .render(),
                        )
                        .await?;
                    let found = query::rows(&agents, "Agent")
                        .iter()
                        .filter_map(|row| {
                            Some((
                                row["_docID"].as_str()?.to_owned(),
                                query::get(row, agent_fields::AGENT_DID)?.to_owned(),
                            ))
                        })
                        .collect::<Vec<_>>();
                    if !found.is_empty() {
                        let doc_ids = found
                            .iter()
                            .map(|(doc_id, _)| doc_id.clone())
                            .collect::<Vec<_>>();
                        let decisions = allowed(
                            txn.observe_document_access(
                                actor,
                                "Agent",
                                &doc_ids,
                                defra_node::DocumentAccessMode::Named {
                                    permission: "update".to_owned(),
                                    node_permission: NodePermission::DocumentUpdate,
                                },
                            )
                            .await?,
                        );
                        configures = found
                            .into_iter()
                            .zip(decisions)
                            .filter_map(|((_, agent_did), held)| held.then_some(agent_did))
                            .collect();
                        configures.sort();
                        configures.dedup();
                    }
                }
                let selection = txn
                    .run(
                        &Query::select(Selection::new().field(default_fields::DEFAULT_AGENT_DID))
                            .filter(Filter::eq(default_fields::NODE_DID, node_did))
                            .limit(1)
                            .render(),
                    )
                    .await?;
                let default_agent_did = query::rows(&selection, "DefaultAgentSelection")
                    .first()
                    .and_then(|row| query::get(row, default_fields::DEFAULT_AGENT_DID))
                    .filter(|did| !did.trim().is_empty())
                    .map(str::to_owned);
                Ok::<_, ConfigClientError>(AgentAdmissionFacts {
                    catalog,
                    configures,
                    default_agent_did,
                })
            })
        })
        .await?;
    Ok(facts)
}

/// What [`observe_document_permissions`] answers for one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedDocumentPermissions {
    pub collection: String,
    pub doc_id: String,
    /// One answer per requested permission, in request order.
    pub held: Vec<bool>,
}

/// Observes, as the runtime, whether `actor` natively holds each named
/// permission of the Household policy on one document: the evaluation
/// [`observe_agent_admission`] reads the catalog with, so an actor asking what
/// it holds gets the answer admission gets. `document` is a collection and
/// document ID, or `None` for this node's Node document; the answer is `None`
/// when that Node document is not found. It decides nothing, and a permission
/// counts as held only when DefraDB answers `Allowed`. This is document ACP's
/// answer (Lean `Identity.Household.allowed`), gated as admission gates it:
/// `read` by the node's document read and every other permission by its
/// document update. The node permission an operation itself needs, such as
/// `delete-document` for a delete, is node access control and is not read here.
pub async fn observe_document_permissions(
    node: &Arc<defra_node::Node>,
    node_did: &str,
    actor: &identity::Did,
    document: Option<(&str, &str)>,
    permissions: &[String],
) -> std::result::Result<Option<ObservedDocumentPermissions>, AgentAdmissionObservationError> {
    use crate::config_client::{ConfigAccess, ConfigClientError, NodePermission};
    use gents_protocol::document_query::{self as query, fields::node as fields, Filter, Query, Selection};
    if node.node_identity_did() != Some(node_did) {
        return Err(AgentAdmissionObservationError::NodeMismatch);
    }
    let runtime = identity::Did::new(node_did).map_err(IdentityError::from)?;
    let observed = ConfigAccess::local_as(node.clone(), runtime)
        .transact_readonly("agent.document_permissions", |txn| {
            Box::pin(async move {
                let (collection, doc_id) = match document {
                    Some((collection, doc_id)) => (collection.to_owned(), doc_id.to_owned()),
                    None => {
                        let nodes = txn
                            .run(
                                &Query::select(Selection::doc_id())
                                    .filter(Filter::eq(fields::NODE_DID, node_did))
                                    .limit(2)
                                    .render(),
                            )
                            .await?;
                        let [row] = query::rows(&nodes, "Node") else {
                            return Ok(None);
                        };
                        let Some(doc_id) = row["_docID"].as_str() else {
                            return Ok(None);
                        };
                        ("Node".to_owned(), doc_id.to_owned())
                    }
                };
                let mut held = Vec::with_capacity(permissions.len());
                for permission in permissions {
                    let node_permission = match permission.as_str() {
                        "read" => NodePermission::DocumentRead,
                        _ => NodePermission::DocumentUpdate,
                    };
                    let decisions = txn
                        .observe_document_access(
                            actor,
                            &collection,
                            std::slice::from_ref(&doc_id),
                            defra_node::DocumentAccessMode::Named {
                                permission: permission.clone(),
                                node_permission,
                            },
                        )
                        .await?;
                    held.push(matches!(
                        decisions.as_slice(),
                        [defra_node::DocumentAccessDecision::Allowed]
                    ));
                }
                Ok::<_, ConfigClientError>(Some(ObservedDocumentPermissions {
                    collection,
                    doc_id,
                    held,
                }))
            })
        })
        .await?;
    Ok(observed)
}

/// Allocate a binding for an explicitly held executing Agent signer. Native
/// creation and relationship operations remain the authorization boundary;
/// the new Agent's baseline is projected when its document publishes.
pub async fn provision_agent_identity_for_agent(
    node: &Arc<defra_node::Node>,
    runtime: &dyn NodeIdentity,
    actor: &dyn NodeIdentity,
    selected_agent_did: &str,
) -> std::result::Result<AgentIdentityBinding, AgentProvisionError> {
    use crate::config_client::ConfigAccess;
    if node.node_identity_did() != Some(runtime.did()) {
        return Err(AgentProvisionError::NodeMismatch);
    }
    if actor.did() != selected_agent_did || actor.did() == runtime.did() {
        return Err(AgentProvisionError::InvalidAgentSigner);
    }
    let caller = identity::Did::new(actor.did()).map_err(IdentityError::from)?;
    let access = ConfigAccess::local_managed_agent(node.clone(), caller, actor)
        .map_err(|_| AgentProvisionError::InvalidAgentSigner)?;
    let rows = access
        .transact_readonly("agent.creator_binding", |txn| {
            Box::pin(async move {
                use gents_protocol::document_query::{
                    fields::agent as fields, Filter, Query, Selection,
                };
                let rendered = Query::select(
                    Selection::new()
                        .field(fields::NODE_DID)
                        .field(fields::AGENT_DID)
                        .field(fields::CREATION_NONCE),
                )
                .filter(Filter::eq(fields::AGENT_DID, selected_agent_did))
                .render();
                let response = txn.run(&rendered).await?;
                Ok::<_, crate::config_client::ConfigClientError>(
                    response["data"]["Agent"].as_array().cloned().unwrap_or_default(),
                )
            })
        })
        .await?;
    let mut nodes = rows
        .iter()
        .filter_map(|row| row["node_did"].as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    nodes.sort();
    nodes.dedup();
    if nodes.len() > 1 {
        return Err(AgentProvisionError::AmbiguousAgent {
            agent_did: selected_agent_did.to_owned(),
            nodes,
        });
    }
    let binding = match rows.as_slice() {
        [row] => row["agent_did"]
            .as_str()
            .zip(row["creation_nonce"].as_str())
            .map(|(agent_did, creation_nonce)| AgentIdentityBinding {
                agent_did: agent_did.to_owned(),
                creation_nonce: creation_nonce.to_owned(),
            }),
        _ => None,
    }
    .ok_or(AgentProvisionError::InvalidAgentSigner)?;
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

/// Proof of which Agent identities a runtime controls: the one thing a store
/// owner may ask of the runtime key it is handed explicitly
/// (`Identity.OwnerStaged.proofFrom`): the same fact whether the signed route
/// or a host-built local access carries it. It never signs.
#[derive(Clone)]
pub struct RuntimeAgentProof {
    runtime_did: String,
    root: AgentDerivationRoot,
}

impl std::fmt::Debug for RuntimeAgentProof {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("RuntimeAgentProof")
            .field(&self.runtime_did)
            .finish()
    }
}

impl RuntimeAgentProof {
    /// The proof of a held runtime key; a key whose root cannot derive Agents
    /// proves none (`foreignAccess`).
    pub fn new(runtime: Arc<dyn NodeIdentity>) -> Option<Self> {
        Self::of(runtime.as_ref())
    }

    pub(crate) fn of(runtime: &dyn NodeIdentity) -> Option<Self> {
        Some(Self {
            runtime_did: runtime.did().to_owned(),
            root: runtime.agent_derivation_root()?,
        })
    }

    /// The DID of the runtime key this proof derives from.
    pub fn runtime_did(&self) -> &str {
        &self.runtime_did
    }

    /// Whether `binding`'s DID derives from this runtime key and its creation
    /// nonce. Only the derived DID is compared; no signer is built.
    pub fn derives(&self, binding: &AgentIdentityBinding) -> bool {
        derive_from_root(&self.root, &self.runtime_did, &binding.creation_nonce)
            .is_ok_and(|derived| derived.did() == binding.agent_did)
    }
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
            runtime_proof: RuntimeAgentProof::of(node),
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
        crate::config_client::ClientSigningCapability,
        crate::config_client::ConfigClientError,
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
    derive_from_root(&held, node.did(), creation_nonce)
}

fn derive_from_root(
    held: &AgentDerivationRoot,
    node_did: &str,
    creation_nonce: &str,
) -> Result<KeyIdentity> {
    if creation_nonce.len() != 64
        || !creation_nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(IdentityError::InvalidAgentCreationNonce);
    }
    let root = &held.identity;
    let derived_did = root.did()?.to_string();
    if derived_did != node_did {
        return Err(IdentityError::RegisteredDidMismatch {
            did: node_did.to_owned(),
            derived_did,
        });
    }
    let root_bytes = Zeroizing::new(root.private_key_bytes());
    let mut context = Vec::with_capacity(16 + node_did.len() + creation_nonce.len());
    for component in [node_did, creation_nonce] {
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
        use gents_lean_vocab::families::agent_identity_derivation as derivation;
        use rapidhash::{HashMapExt, RapidHashMap};
        let directory = tempfile::tempdir().unwrap();
        let mut roots = RapidHashMap::new();
        for case in gents_lean_vocab::families::agent_identity_derivation::cases() {
            let mut derive = |input: &derivation::DerivationInput| {
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
            let left = derive(&case.input.left);
            let right = derive(&case.input.right);
            case.check(derivation::DerivationOutput {
                same_did: left.did() == right.did(),
            });
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
