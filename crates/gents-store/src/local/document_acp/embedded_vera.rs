use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use defra_node::keyring::{self, FileKeyring, Keyring};

use defra_node::{EmbeddedVeraActor, EmbeddedVeraConfig, EmbeddedVeraDeployment};
use sha2::{Digest as _, Sha256};
use vera::vera_rs::{DelegationAlgorithm, DelegationSigner, SignerError};
use zeroize::Zeroizing;

use crate::identity::NodeIdentity;
use crate::local::store_key::StoreKey;

#[cfg(test)]
mod tests;

const WORKER_DIR_NAME: &str = "worker";
const KEYS_DIR_NAME: &str = "keys";
const START_DEADLINE: Duration = Duration::from_secs(60);
const STOP_DEADLINE: Duration = Duration::from_secs(10);
const CHAIN_ID_DOMAIN: &[u8] = b"gents embedded vera deployment v1\0";
const KEYRING_DOMAIN: &[u8] = b"gents embedded vera worker keyring v1";

/// A deployment's chain identifier is fixed when its directory is first used
/// and checked on every reopen, so it is a function of the node's DID alone.
/// It stays below 2^53 so that a client reading it as a JSON number keeps it
/// exact.
fn chain_id(node_did: &str) -> u64 {
    let digest = Sha256::new()
        .chain_update(CHAIN_ID_DOMAIN)
        .chain_update(node_did.as_bytes())
        .finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) & ((1 << 53) - 1)).max(1)
}

fn deployment(node_did: &str) -> EmbeddedVeraDeployment {
    EmbeddedVeraDeployment {
        chain_id: chain_id(node_did),
        threshold: 1,
        operator_keys: vec![node_did.to_owned()],
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
        policies: crate::schema::catalog::NODE_POLICIES
            .iter()
            .map(crate::schema::catalog::vera_seed::seed)
            .collect(),
    }
}

struct NodeSigner {
    node: Arc<dyn NodeIdentity>,
}

impl DelegationSigner for NodeSigner {
    fn did(&self) -> &str {
        self.node.did()
    }

    fn algorithm(&self) -> DelegationAlgorithm {
        DelegationAlgorithm::EdDsa
    }

    fn sign<'a>(
        &'a self,
        signing_input: &'a [u8],
    ) -> defra_core::thread_bounds::MaybeBoxFuture<'a, std::result::Result<Vec<u8>, SignerError>>
    {
        Box::pin(async move {
            self.node.sign(signing_input).await.map_err(|error| {
                SignerError::Failed(gents_protocol::error_chain::ErrorChain(&error).to_string())
            })
        })
    }
}

/// Opened on first use. The store's data directory must hold nothing when
/// DefraDB creates the store in it, so nothing of the Vera node's may be
/// created before the node is built; DefraDB opens the store first and
/// reaches the worker's keyring afterwards.
struct WorkerKeyring {
    directory: PathBuf,
    password: Zeroizing<Vec<u8>>,
    opened: OnceLock<FileKeyring>,
}

impl WorkerKeyring {
    fn new(directory: PathBuf, key: &StoreKey) -> Self {
        let mut password = Zeroizing::new(vec![0_u8; 32]);
        key.with_secret(|secret| {
            hkdf::Hkdf::<Sha256>::new(None, secret)
                .expand(KEYRING_DOMAIN, &mut password)
                .expect("HKDF-SHA256 expands to 32 bytes")
        });
        Self {
            directory,
            password,
            opened: OnceLock::new(),
        }
    }

    fn opened(&self) -> keyring::Result<&FileKeyring> {
        if let Some(opened) = self.opened.get() {
            return Ok(opened);
        }
        let opened = FileKeyring::open(&self.directory, self.password.to_vec())?;
        Ok(self.opened.get_or_init(|| opened))
    }
}

impl Keyring for WorkerKeyring {
    fn set(&self, name: &str, key: &[u8]) -> keyring::Result<()> {
        self.opened()?.set(name, key)
    }

    fn get(&self, name: &str) -> keyring::Result<Zeroizing<Vec<u8>>> {
        self.opened()?.get(name)
    }

    fn delete(&self, name: &str) -> keyring::Result<()> {
        self.opened()?.delete(name)
    }

    fn list(&self) -> keyring::Result<Vec<String>> {
        self.opened()?.list()
    }
}

pub(super) fn deployment_differs(error: &defra_node::Error) -> Option<&'static str> {
    match error {
        defra_node::Error::EmbeddedVera(vera_embedded::HostError::Start(
            vera_node::EmbedError::Deployment(vera_node::SoloError::Mismatch(what)),
        )) => Some(what),
        _ => None,
    }
}

pub(super) fn consensus_key(node: &defra_node::Node) -> Option<String> {
    node.embedded_vera_trust()
        .map(|deployment| deployment.consensus_key.clone())
}

pub(super) fn provider_time(node: &defra_node::Node) -> Option<u64> {
    use acp::RemotePolicies as _;
    node.vera_acp()?.light_client_status().ok()?.chain_time
}

pub(super) fn delegation_audience(
    node: &defra_node::Node,
) -> Option<gents_protocol::delegation_audience::DelegationAudience> {
    use acp::RemotePolicies as _;
    let audience = node.vera_acp()?.light_client_status().ok()?.delegation?;
    Some(gents_protocol::delegation_audience::DelegationAudience {
        worker: audience.worker,
        deployment: audience.deployment,
        genesis_id: audience
            .genesis_id
            .map(|genesis| format!("0x{}", hex::encode(genesis))),
    })
}

pub(super) fn configure(
    builder: defra_node::NodeBuilder,
    node_dir: &Path,
    root: &Path,
    key: &StoreKey,
    node: &Arc<dyn NodeIdentity>,
) -> defra_node::NodeBuilder {
    builder.with_embedded_vera(EmbeddedVeraConfig {
        data_dir: node_dir.to_path_buf(),
        deployment: deployment(node.did()),
        listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        start_deadline: START_DEADLINE,
        stop_deadline: STOP_DEADLINE,
        actor: EmbeddedVeraActor::Signer(Arc::new(NodeSigner {
            node: Arc::clone(node),
        })),
        rpc_listen: None,
        worker_dir: root.join(WORKER_DIR_NAME),
        keyring: Arc::new(WorkerKeyring::new(root.join(KEYS_DIR_NAME), key)),
        tuning: vera::AcpTuning::default(),
    })
}
