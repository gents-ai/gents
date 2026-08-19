use std::collections::BTreeSet;

use anyhow::{bail, Result};
use gents::agent::p2p_reconcile::templates::{equality_filter, PairingFilters};
use gents::agent::p2p_reconcile::{
    compute_owned_pairing_diff, update_applied_after_success, DiffOp,
    PairingActual as RuntimePairingActual,
};
use gents::graphql::escape_graphql_string;
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
    applied_replicator_filter: PairingFilters,
    drop_next_reconcile: bool,
    desired_replicator_filter: PairingFilters,
}

pub struct Harness {
    a: HarnessNode,
    b: HarnessNode,
    history: Vec<ObservedSnapshot>,
    emitted_ops: Vec<DiffOp>,
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
            applied_replicator_filter: PairingFilters::default(),
            drop_next_reconcile: false,
            desired_replicator_filter: PairingFilters::default(),
        };
        let b = HarnessNode {
            id: "B".to_string(),
            db: test_db("pairing-conformance-b").await,
            actual_collections: BTreeSet::new(),
            actual_replicator_addresses: BTreeSet::new(),
            actual_connected: false,
            applied_collections: BTreeSet::new(),
            applied_replicator_addresses: BTreeSet::new(),
            applied_replicator_filter: PairingFilters::default(),
            drop_next_reconcile: false,
            desired_replicator_filter: PairingFilters::default(),
        };
        let mut harness = Self {
            a,
            b,
            history: Vec::new(),
            emitted_ops: Vec::new(),
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

    pub fn emitted_ops(&self) -> &[DiffOp] {
        &self.emitted_ops
    }

    async fn apply_action(&mut self, action: &Action) -> Result<()> {
        match action {
            Action::OperatorWrite {
                node,
                peer,
                collections,
                replicator_addresses,
                replicator_filter,
            } => {
                self.node_mut(node)?.desired_replicator_filter =
                    scenario_filter_to_pairing_filters(replicator_filter);
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
            Action::Drop { node } => {
                self.node_mut(node)?.drop_next_reconcile = true;
            }
            Action::Crash { node } => {
                self.crash_node(node).await?;
                self.record_observation().await?;
            }
            Action::WaitForConvergence => {
                self.wait_for_convergence().await?;
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
        let ops = if node_id == "A" {
            apply_desired_state(&mut self.a, &mut self.b, &desired, peer_id).await?
        } else {
            apply_desired_state(&mut self.b, &mut self.a, &desired, peer_id).await?
        };
        self.emitted_ops.extend(ops);
        Ok(ReconcileOutcome::Applied)
    }

    async fn wait_for_convergence(&mut self) -> Result<()> {
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

    async fn crash_node(&mut self, id: &NodeId) -> Result<()> {
        let peer_id = peer_id_for_reconciler(id)?;
        let node = self.node_mut(id)?;
        node.drop_next_reconcile = false;
        node.applied_collections.clear();
        node.applied_replicator_addresses.clear();
        let applied = read_peer_pairing_applied(node, peer_id).await?;
        node.applied_collections = applied.collections;
        node.applied_replicator_addresses = applied.replicator_addresses;
        Ok(())
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
                replicator_filter: self.a.applied_replicator_filter.clone(),
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

async fn apply_desired_state(
    reconciler: &mut HarnessNode,
    peer: &mut HarnessNode,
    desired: &PairingDesired,
    peer_id: &str,
) -> Result<Vec<DiffOp>> {
    if desired.has_wiring() && !peer.actual_connected {
        peer.actual_connected = true;
    }

    let actual = RuntimePairingActual {
        collections: peer.actual_collections.clone(),
        replicator_addresses: peer.actual_replicator_addresses.clone(),
        ..Default::default()
    };
    let mut applied = PairingApplied {
        collections: reconciler.applied_collections.clone(),
        replicator_addresses: reconciler.applied_replicator_addresses.clone(),
        replicator_filter: reconciler.applied_replicator_filter.clone(),
    };
    let ops = compute_owned_pairing_diff(desired, &actual, &applied);
    for op in &ops {
        apply_op_to_actual(peer, op);
        update_applied_after_success(&mut applied, op, desired);
        reconciler.applied_replicator_filter = applied.replicator_filter.clone();
        reconciler.applied_collections = applied.collections.clone();
        reconciler.applied_replicator_addresses = applied.replicator_addresses.clone();
        write_peer_pairing_applied(reconciler, peer_id, &applied).await?;
    }
    Ok(ops)
}

fn apply_op_to_actual(peer: &mut HarnessNode, op: &DiffOp) {
    match op {
        DiffOp::InstallCollection(collection) => {
            peer.actual_collections.insert(collection.clone());
        }
        DiffOp::TeardownCollection(collection) => {
            peer.actual_collections.remove(collection);
        }
        DiffOp::InstallReplicator(address) => {
            peer.actual_replicator_addresses.insert(address.clone());
        }
        DiffOp::TeardownReplicator(address) => {
            peer.actual_replicator_addresses.remove(address);
        }
    }
}

#[derive(Deserialize)]
struct DesiredRow {
    collections: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct AppliedRow {
    #[serde(rename = "_docID")]
    doc_id: String,
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
        replicator_filter: node.desired_replicator_filter.clone(),
        ..Default::default()
    })
}

fn scenario_filter_to_pairing_filters(
    filter: &std::collections::BTreeMap<String, super::scenario::ScenarioFilterPredicate>,
) -> PairingFilters {
    filter
        .iter()
        .map(|(collection, predicate)| {
            (
                collection.clone(),
                equality_filter(predicate.field.clone(), predicate.value.clone()),
            )
        })
        .collect()
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

async fn read_peer_pairing_applied(node: &HarnessNode, peer: &str) -> Result<PairingApplied> {
    let row = read_peer_pairing_applied_row(node, peer).await?;
    Ok(PairingApplied {
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
        ..Default::default()
    })
}

async fn read_peer_pairing_applied_row(
    node: &HarnessNode,
    peer: &str,
) -> Result<Option<AppliedRow>> {
    let peer = escape_graphql_string(peer);
    let query = format!(
        r#"{{
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer}" }} }}) {{
                _docID
                collections
                replicator_addresses
            }}
        }}"#
    );
    let resp = node.db.node.execute(&query).await;
    if resp.has_errors() {
        bail!("read PeerPairingApplied failed: {:?}", resp.errors);
    }
    Ok(
        gents::graphql::rows::<AppliedRow>(&resp, "PeerPairingApplied")?
            .into_iter()
            .min_by(|left, right| left.doc_id.cmp(&right.doc_id)),
    )
}

async fn write_peer_pairing_applied(
    node: &HarnessNode,
    peer: &str,
    applied: &PairingApplied,
) -> Result<()> {
    let escaped_peer = escape_graphql_string(peer);
    let mutation = if applied.is_empty() {
        format!(
            r#"mutation {{
                delete_PeerPairingApplied(
                    filter: {{ peer_id: {{ _eq: "{escaped_peer}" }} }}
                ) {{ _docID }}
            }}"#
        )
    } else {
        let existing_doc_id = read_peer_pairing_applied_row(node, peer)
            .await?
            .map(|row| row.doc_id);
        let collections = graphql_string_set_literal(&applied.collections);
        let replicator_addresses = graphql_string_set_literal(&applied.replicator_addresses);
        let now = chrono::Utc::now().to_rfc3339();
        let now = escape_graphql_string(&now);
        match existing_doc_id {
            Some(doc_id) => format!(
                r#"mutation {{
                    update_PeerPairingApplied(
                        filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                        input: {{
                            collections: {collections},
                            replicator_addresses: {replicator_addresses},
                            updated_at: "{now}"
                        }}
                    ) {{ _docID }}
                }}"#,
                doc_id = escape_graphql_string(&doc_id),
            ),
            None => format!(
                r#"mutation {{
                    create_PeerPairingApplied(input: {{
                        peer_id: "{escaped_peer}",
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        created_at: "{now}",
                        updated_at: "{now}"
                    }}) {{ _docID }}
                }}"#,
            ),
        }
    };
    let resp = node.db.node.execute(&mutation).await;
    if resp.has_errors() {
        bail!("write PeerPairingApplied failed: {:?}", resp.errors);
    }
    Ok(())
}

fn peer_id_for_reconciler(node: &NodeId) -> Result<&'static str> {
    if node == "A" {
        Ok("B")
    } else if node == "B" {
        Ok("A")
    } else {
        bail!("unknown node {node}")
    }
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

fn graphql_string_set_literal(values: &BTreeSet<String>) -> String {
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
