use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pack::home_install::{recorded_provenance, InstalledVia};
use crate::pack::resolved_pack::ResolvedPack;
use crate::pack::wire_subject::WireSubject;
use crate::pack::PackKind;

/// The file under a client's root that keeps the packs it approved.
pub const APPROVALS_FILE: &str = "pack-approvals.json";

/// The largest approvals record a client reads.
pub const MAX_APPROVALS_BYTES: u64 = 1024 * 1024;

const APPROVALS_VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ApprovalsError {
    #[error("accessing this device's approved packs")]
    Io(#[from] std::io::Error),
    #[error(
        "this device's approved packs record exceeds {MAX_APPROVALS_BYTES} bytes; it is not read"
    )]
    Oversized,
    #[error("this device's approved packs record cannot be read; it is not taken as empty")]
    Unparseable(#[source] serde_json::Error),
    #[error("this device's approved packs record holds subjects of another pack than the approval names; it is not read")]
    Inconsistent,
    #[error("another command is recording an approval on this device; retry")]
    Busy,
    #[error("encoding this device's approved packs")]
    Encode(#[source] serde_json::Error),
    #[error("deriving what the pack yields")]
    Pack(#[source] anyhow::Error),
    #[error("the approved packs operation ended unexpectedly")]
    Task(#[from] tokio::task::JoinError),
}

/// One pack a client approved for a node (`Configuration.PackApproval`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackApproval {
    pub digest: String,
    pub coordinate: String,
    pub version: String,
    pub kind: PackKind,
    pub approved_at: String,
    pub subjects: Vec<WireSubject>,
}

impl PackApproval {
    /// What the client approves of `pack`, which it resolved itself.
    pub fn of_pack(pack: &ResolvedPack) -> Result<Self, ApprovalsError> {
        let manifest = pack.manifest();
        Ok(Self {
            digest: pack.digest().to_owned(),
            coordinate: format!("{}/{}", manifest.metadata.namespace, manifest.name),
            version: manifest.version.clone(),
            kind: manifest.metadata.kind.clone(),
            approved_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            subjects: WireSubject::of_sent_pack(pack).map_err(ApprovalsError::Pack)?,
        })
    }

    /// Who installed the pack, as a wire's targets read it: the client that
    /// approved it, as an install record without a provenance names it.
    pub fn provenance(&self) -> InstalledVia {
        recorded_provenance(self.kind.clone(), Some(None))
    }
}

/// The record, as lists so that a node or digest written twice is seen and
/// refused rather than silently collapsed.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u8,
    nodes: Vec<NodeApprovals>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeApprovals {
    node_did: String,
    approvals: Vec<PackApproval>,
}

/// The approvals a client keeps under its `root`.
pub struct Approvals {
    path: PathBuf,
}

impl Approvals {
    pub fn under(root: &Path) -> Self {
        Self {
            path: root.join(APPROVALS_FILE),
        }
    }

    /// What the client approved for `node`, by pack digest
    /// (`Configuration.approvedSubjects`).
    pub async fn for_node(
        &self,
        node: &str,
    ) -> Result<BTreeMap<String, PackApproval>, ApprovalsError> {
        let (path, node) = (self.path.clone(), node.to_owned());
        tokio::task::spawn_blocking(move || {
            Ok(read(&path)?
                .nodes
                .into_iter()
                .find(|kept| kept.node_did == node)
                .map(|kept| {
                    kept.approvals
                        .into_iter()
                        .map(|approval| (approval.digest.clone(), approval))
                        .collect()
                })
                .unwrap_or_default())
        })
        .await?
    }

    /// The subjects a served record may match for `node`.
    pub async fn subjects(&self, node: &str) -> Result<Vec<WireSubject>, ApprovalsError> {
        Ok(self
            .for_node(node)
            .await?
            .into_values()
            .flat_map(|approval| approval.subjects)
            .collect())
    }

    /// Record `approval` for `node`, replacing an approval of the same pack
    /// digest. The record is rewritten whole under the client's lock and
    /// replaces the old one only once it is durable.
    pub async fn record(&self, node: &str, approval: PackApproval) -> Result<(), ApprovalsError> {
        let (path, node) = (self.path.clone(), node.to_owned());
        tokio::task::spawn_blocking(move || {
            let parent = path.parent().ok_or(ApprovalsError::Inconsistent)?;
            std::fs::create_dir_all(parent)?;
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path.with_extension("lock"))?;
            match lease.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Err(ApprovalsError::Busy),
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
            let mut stored = read(&path)?;
            let index = match stored.nodes.iter().position(|kept| kept.node_did == node) {
                Some(index) => index,
                None => {
                    stored.nodes.push(NodeApprovals {
                        node_did: node,
                        approvals: Vec::new(),
                    });
                    stored.nodes.len() - 1
                }
            };
            let approvals = &mut stored.nodes[index].approvals;
            approvals.retain(|kept| kept.digest != approval.digest);
            approvals.push(approval);
            check(&stored)?;
            let encoded = serde_json::to_vec(&stored).map_err(ApprovalsError::Encode)?;
            if encoded.len() as u64 > MAX_APPROVALS_BYTES {
                return Err(ApprovalsError::Oversized);
            }
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            std::io::Write::write_all(file.as_file_mut(), &encoded)?;
            file.as_file().sync_all()?;
            file.persist(&path).map_err(|error| error.error)?;
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            Ok(())
        })
        .await?
    }
}

fn read(path: &Path) -> Result<Stored, ApprovalsError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Stored {
                version: APPROVALS_VERSION,
                nodes: Vec::new(),
            })
        }
        Err(error) => return Err(error.into()),
    };
    let mut data = Vec::new();
    file.take(MAX_APPROVALS_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_APPROVALS_BYTES {
        return Err(ApprovalsError::Oversized);
    }
    let stored: Stored = serde_json::from_slice(&data).map_err(ApprovalsError::Unparseable)?;
    if stored.version != APPROVALS_VERSION {
        return Err(ApprovalsError::Inconsistent);
    }
    check(&stored)?;
    Ok(stored)
}

/// `Configuration.approvalsConsistent`, for each node: each subject names
/// its approval's pack, and no digest is approved twice; no node is kept
/// twice.
fn check(stored: &Stored) -> Result<(), ApprovalsError> {
    let mut nodes = std::collections::BTreeSet::new();
    let consistent = stored.nodes.iter().all(|kept| {
        let mut digests = std::collections::BTreeSet::new();
        nodes.insert(kept.node_did.as_str())
            && kept.approvals.iter().all(|approval| {
                digests.insert(approval.digest.as_str())
                    && approval.subjects.iter().all(|subject| {
                        subject.owner_pack_digest == approval.digest
                            && subject.owner_pack_coordinate == approval.coordinate
                    })
            })
    });
    if consistent {
        Ok(())
    } else {
        Err(ApprovalsError::Inconsistent)
    }
}

#[cfg(test)]
mod tests;
