//! Two-node orchestration scaffold for pairing reconcile conformance.
//!
//! Desired state is written through real embedded DefraDB nodes. The production
//! supervisor and HTTP remote-admin implementation are covered in
//! `defra-agent-desktop-core`; this harness keeps the TLA-style scenario IR
//! executable from `defra-agent` without introducing a workspace dependency
//! cycle back to desktop-core.

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{bail, Result};
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;

use crate::support::{first_optional_row, test_db, TestDb};

use super::invariants::ObservedSnapshot;
use super::scenario::{Action, NodeId, Scenario};
use super::{PairingActual, PairingApplied, PairingDesired};

pub struct HarnessNode {
    pub id: NodeId,
    pub db: TestDb,
    actual_collections: BTreeSet<String>,
    actual_replicator_addresses: BTreeSet<String>,
    actual_connected: bool,
    applied_collections: BTreeSet<String>,
    applied_replicator_addresses: BTreeSet<String>,
    drop_next_reconcile: bool,
}

pub struct Harness {
    a: HarnessNode,
    b: HarnessNode,
    history: Vec<ObservedSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconcileOutcome {
    Applied,
    ReadFailed,
}

impl ReconcileOutcome {
    fn read_failed(self) -> bool {
        matches!(self, Self::ReadFailed)
    }
}

impl Harness {
    pub async fn start_two_nodes() -> Result<Self> {
        let a = HarnessNode {
            id: "A".to_string(),
            db: test_db("pairing-conformance-a").await,
            actual_collections: BTreeSet::new(),
            actual_replicator_addresses: BTreeSet::new(),
            actual_connected: false,
            applied_collections: BTreeSet::new(),
            applied_replicator_addresses: BTreeSet::new(),
            drop_next_reconcile: false,
        };
        let b = HarnessNode {
            id: "B".to_string(),
            db: test_db("pairing-conformance-b").await,
            actual_collections: BTreeSet::new(),
            actual_replicator_addresses: BTreeSet::new(),
            actual_connected: false,
            applied_collections: BTreeSet::new(),
            applied_replicator_addresses: BTreeSet::new(),
            drop_next_reconcile: false,
        };
        let mut harness = Self {
            a,
            b,
            history: Vec::new(),
        };
        harness.record_observation().await?;
        Ok(harness)
    }

    pub async fn run(&mut self, scenario: &Scenario) -> Result<()> {
        for action in &scenario.actions {
            self.apply_action(action).await?;
        }
        Ok(())
    }

    pub fn observation_history(&self) -> Vec<ObservedSnapshot> {
        self.history.clone()
    }

    async fn apply_action(&mut self, action: &Action) -> Result<()> {
        match action {
            Action::OperatorWrite {
                node,
                peer,
                collections,
                replicator_addresses,
            } => {
                write_peer_pairing_desired(
                    self.node(node)?,
                    peer,
                    collections,
                    replicator_addresses,
                )
                .await?;
                self.record_observation().await?;
            }
            Action::OperatorDelete { node, peer } => {
                delete_peer_pairing_desired(self.node(node)?, peer).await?;
                self.record_observation().await?;
            }
            Action::Reconcile { node } => {
                let read_failed = self.reconcile_node(node).await?.read_failed();
                self.record_observation_with_read_failed(read_failed)
                    .await?;
            }
            Action::ReadFailure { node } => {
                let _ = self.node(node)?;
                self.record_observation_with_read_failed(true).await?;
            }
            Action::PreseedActual {
                node,
                collections,
                replicator_addresses,
                connected,
            } => {
                let node = self.node_mut(node)?;
                node.actual_collections.extend(collections.iter().cloned());
                node.actual_replicator_addresses
                    .extend(replicator_addresses.iter().cloned());
                node.actual_connected = *connected;
                self.record_observation().await?;
            }
            Action::PeerDisconnected { node } => {
                self.node_mut(node)?.actual_connected = false;
                self.record_observation().await?;
            }
            Action::Drop { node } => {
                self.node_mut(node)?.drop_next_reconcile = true;
            }
            Action::Crash { node } => {
                self.node_mut(node)?.drop_next_reconcile = false;
                self.record_observation().await?;
            }
            Action::WaitForConvergence { timeout_secs } => {
                self.wait_for_convergence(Duration::from_secs(*timeout_secs))
                    .await?;
            }
        }
        Ok(())
    }

    async fn reconcile_node(&mut self, node_id: &NodeId) -> Result<ReconcileOutcome> {
        let peer_id = if node_id == "A" {
            "B"
        } else if node_id == "B" {
            "A"
        } else {
            bail!("unknown node {node_id}");
        };
        let desired = {
            let node = self.node_mut(node_id)?;
            if node.drop_next_reconcile {
                node.drop_next_reconcile = false;
                return Ok(ReconcileOutcome::ReadFailed);
            }
            read_desired_state(node, peer_id).await?
        };
        if node_id == "A" {
            apply_desired_state(&mut self.a, &mut self.b, &desired);
        } else {
            apply_desired_state(&mut self.b, &mut self.a, &desired);
        }
        Ok(ReconcileOutcome::Applied)
    }

    async fn wait_for_convergence(&mut self, _timeout: Duration) -> Result<()> {
        let snapshot = self.current_snapshot().await?;
        if crate::support::pairing_conformance::invariants::check_liveness(&snapshot) {
            self.history.push(snapshot);
            return Ok(());
        }

        let _ = self.reconcile_node(&"A".to_string()).await?;
        let _ = self.reconcile_node(&"B".to_string()).await?;
        self.record_observation().await?;

        let snapshot = self.current_snapshot().await?;
        if crate::support::pairing_conformance::invariants::check_liveness(&snapshot) {
            Ok(())
        } else {
            bail!(
                "convergence timeout: desired={:?} actual={:?} applied={:?}",
                snapshot.desired,
                snapshot.actual,
                snapshot.applied
            )
        }
    }

    fn node_mut(&mut self, id: &NodeId) -> Result<&mut HarnessNode> {
        if id == &self.a.id {
            Ok(&mut self.a)
        } else if id == &self.b.id {
            Ok(&mut self.b)
        } else {
            bail!("unknown node {id}")
        }
    }

    fn node(&self, id: &NodeId) -> Result<&HarnessNode> {
        if id == &self.a.id {
            Ok(&self.a)
        } else if id == &self.b.id {
            Ok(&self.b)
        } else {
            bail!("unknown node {id}")
        }
    }

    async fn current_snapshot(&self) -> Result<ObservedSnapshot> {
        Ok(ObservedSnapshot {
            desired: read_desired_state(&self.a, "B").await?,
            actual: PairingActual {
                collections: self.b.actual_collections.clone(),
                replicator_addresses: self.b.actual_replicator_addresses.clone(),
                connected: self.b.actual_connected,
            },
            applied: PairingApplied {
                collections: self.a.applied_collections.clone(),
                replicator_addresses: self.a.applied_replicator_addresses.clone(),
            },
            read_failed: false,
        })
    }

    async fn record_observation(&mut self) -> Result<()> {
        self.record_observation_with_read_failed(false).await
    }

    async fn record_observation_with_read_failed(&mut self, read_failed: bool) -> Result<()> {
        let snapshot = self.current_snapshot().await?;
        self.history.push(ObservedSnapshot {
            read_failed,
            ..snapshot
        });
        Ok(())
    }
}

fn apply_desired_state(
    reconciler: &mut HarnessNode,
    peer: &mut HarnessNode,
    desired: &PairingDesired,
) {
    if desired.has_wiring() && !peer.actual_connected {
        peer.actual_connected = true;
    }

    for collection in desired.collections.iter() {
        if peer.actual_collections.insert(collection.clone()) {
            reconciler.applied_collections.insert(collection.clone());
        }
    }
    for replicator in desired.replicator_addresses.iter() {
        if peer.actual_replicator_addresses.insert(replicator.clone()) {
            reconciler
                .applied_replicator_addresses
                .insert(replicator.clone());
        }
    }

    let collections_to_remove = reconciler
        .applied_collections
        .difference(&desired.collections)
        .cloned()
        .collect::<Vec<_>>();
    for collection in collections_to_remove {
        peer.actual_collections.remove(&collection);
        reconciler.applied_collections.remove(&collection);
    }

    let replicators_to_remove = reconciler
        .applied_replicator_addresses
        .difference(&desired.replicator_addresses)
        .cloned()
        .collect::<Vec<_>>();
    for replicator in replicators_to_remove {
        peer.actual_replicator_addresses.remove(&replicator);
        reconciler.applied_replicator_addresses.remove(&replicator);
    }
}

#[derive(Deserialize)]
struct DesiredRow {
    collections: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct DesiredDocRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    created_at: Option<String>,
}

async fn read_desired_state(node: &HarnessNode, peer: &str) -> Result<PairingDesired> {
    let peer = escape_graphql_string(peer);
    let query = format!(
        r#"{{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer}" }} }}) {{
                collections
                replicator_addresses
            }}
        }}"#
    );
    let resp = node.db.node.execute(&query).await;
    let row = first_optional_row::<DesiredRow>(&resp, "PeerPairingDesired");
    Ok(PairingDesired {
        collections: row
            .as_ref()
            .and_then(|row| row.collections.clone())
            .unwrap_or_default()
            .into_iter()
            .collect(),
        replicator_addresses: row
            .and_then(|row| row.replicator_addresses)
            .unwrap_or_default()
            .into_iter()
            .collect(),
    })
}

async fn write_peer_pairing_desired(
    node: &HarnessNode,
    peer: &str,
    collections: &[String],
    replicator_addresses: &[String],
) -> Result<()> {
    let peer = escape_graphql_string(peer);
    let doc_query = format!(
        r#"{{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer}" }} }}) {{
                _docID
                created_at
            }}
        }}"#
    );
    let doc_resp = node.db.node.execute(&doc_query).await;
    let doc = first_optional_row::<DesiredDocRow>(&doc_resp, "PeerPairingDesired");
    let collections = graphql_string_array(collections);
    let replicator_addresses = graphql_string_array(replicator_addresses);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let now = escape_graphql_string(&now);
    let mutation = if let Some(doc) = doc {
        let doc_id = escape_graphql_string(&doc.doc_id);
        let created_at = doc
            .created_at
            .as_deref()
            .map(escape_graphql_string)
            .unwrap_or_else(|| now.clone());
        format!(
            r#"mutation {{
                update_PeerPairingDesired(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        created_at: "{created_at}",
                        updated_at: "{now}"
                    }}
                ) {{ _docID }}
            }}"#
        )
    } else {
        format!(
            r#"mutation {{
                create_PeerPairingDesired(input: {{
                    peer_id: "{peer}",
                    collections: {collections},
                    replicator_addresses: {replicator_addresses},
                    created_at: "{now}",
                    updated_at: "{now}"
                }}) {{ _docID }}
            }}"#
        )
    };
    let resp = node.db.node.execute(&mutation).await;
    if resp.has_errors() {
        bail!("write PeerPairingDesired failed: {:?}", resp.errors);
    }
    Ok(())
}

async fn delete_peer_pairing_desired(node: &HarnessNode, peer: &str) -> Result<()> {
    let peer = escape_graphql_string(peer);
    let mutation = format!(
        r#"mutation {{
            delete_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer}" }} }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.db.node.execute(&mutation).await;
    if resp.has_errors() {
        bail!("delete PeerPairingDesired failed: {:?}", resp.errors);
    }
    Ok(())
}

fn graphql_string_array(values: &[String]) -> String {
    if values.is_empty() {
        return "null".to_string();
    }

    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
