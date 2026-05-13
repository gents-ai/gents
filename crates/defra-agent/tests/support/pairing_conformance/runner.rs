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
use super::{PairingActual, PairingDesired};

pub struct HarnessNode {
    pub id: NodeId,
    pub db: TestDb,
    actual_collections: BTreeSet<String>,
    drop_next_reconcile: bool,
}

pub struct Harness {
    a: HarnessNode,
    b: HarnessNode,
    history: Vec<ObservedSnapshot>,
}

impl Harness {
    pub async fn start_two_nodes() -> Result<Self> {
        let a = HarnessNode {
            id: "A".to_string(),
            db: test_db("pairing-conformance-a").await,
            actual_collections: BTreeSet::new(),
            drop_next_reconcile: false,
        };
        let b = HarnessNode {
            id: "B".to_string(),
            db: test_db("pairing-conformance-b").await,
            actual_collections: BTreeSet::new(),
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
            } => {
                write_peer_pairing_desired(self.node(node)?, peer, collections).await?;
                self.record_observation().await?;
            }
            Action::Reconcile { node } => {
                self.reconcile_node(node).await?;
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

    async fn reconcile_node(&mut self, node_id: &NodeId) -> Result<()> {
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
                return Ok(());
            }
            read_desired_state(node, peer_id).await?
        };
        if node_id == "A" {
            self.b.actual_collections = desired.collections;
        } else {
            self.a.actual_collections = desired.collections;
        }
        Ok(())
    }

    async fn wait_for_convergence(&mut self, _timeout: Duration) -> Result<()> {
        let snapshot = self.current_snapshot().await?;
        if snapshot.desired.collections == snapshot.actual.collections {
            self.history.push(snapshot);
            return Ok(());
        }

        self.reconcile_node(&"A".to_string()).await?;
        self.reconcile_node(&"B".to_string()).await?;
        self.record_observation().await?;

        let snapshot = self.current_snapshot().await?;
        if snapshot.desired.collections == snapshot.actual.collections {
            Ok(())
        } else {
            bail!(
                "convergence timeout: desired={:?} actual={:?}",
                snapshot.desired.collections,
                snapshot.actual.collections
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
                replicator_addresses: Default::default(),
            },
        })
    }

    async fn record_observation(&mut self) -> Result<()> {
        let snapshot = self.current_snapshot().await?;
        self.history.push(snapshot);
        Ok(())
    }
}

#[derive(Deserialize)]
struct DesiredRow {
    collections: Vec<String>,
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
            .map(|row| row.collections.into_iter().collect())
            .unwrap_or_default(),
        replicator_addresses: Default::default(),
    })
}

async fn write_peer_pairing_desired(
    node: &HarnessNode,
    peer: &str,
    collections: &[String],
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
                        collections: [{collections}],
                        replicator_addresses: [],
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
                    collections: [{collections}],
                    replicator_addresses: [],
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

fn graphql_string_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ")
}
