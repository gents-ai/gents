mod materialize;

pub use materialize::materialize_enrollment_desired_in_txn;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use defra_node::Node as EmbeddedNode;

use crate::store::config_client::ConfigAccess;
use crate::store::graphql::{escape_graphql_string, fence_arrivals};
use crate::store::identity::NodeIdentity;

use super::super::enrollment_reconcile::EnrollmentAuthorityHandle;
use super::super::graphql_helpers::{
    first_row, graphql_nullable_string_literal, graphql_string_list_literal, rows,
};
use super::super::PairingDesired;
use super::desired::{data_plane_desired_from_pairing_row, merge_layered_desired};
use super::enrollment::{
    enrollment_base_row, enrollment_entry_is_fresh_at, local_data_plane_row,
    materialized_enrollment_entry, MaterializedDataPlaneEntry,
};
use super::rows::{
    graphql_nullable_filter_literal, graphql_nullable_string_array, AppliedStateRow,
    PairingStateRow, PeerIdRow,
};
use super::{
    EnrollmentEndpointEntry, EnrollmentRouteGeneration, LoadedPairingApplied, LoadedPairingDesired,
    PairingStateStore,
};

#[derive(Clone)]
pub struct GraphqlPairingStateStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    enrollment: Option<EnrollmentAuthorityHandle>,
    exact_enrollment: Option<EnrollmentEndpointEntry>,
}

impl GraphqlPairingStateStore {
    fn runtime_access(&self) -> Result<ConfigAccess> {
        anyhow::ensure!(
            self.node.node_identity_did() == Some(self.identity.did()),
            "pairing state requires the configured runtime identity"
        );
        Ok(ConfigAccess::local_as(
            self.node.clone(),
            identity::Did::new(self.identity.did())?,
        ))
    }

    pub(super) async fn runtime_query(&self, query: &str) -> Result<defra_node::QueryResponse> {
        self.runtime_access()?
            .transact_readonly("p2p.query_pairing_state", |txn| {
                Box::pin(async move { txn.execute_local_response(query).await })
            })
            .await
            .map_err(Into::into)
    }

    async fn verify_applied_owners(
        txn: &crate::store::config_client::ConfigApplyTxn<'_>,
        peer_id: &str,
        selected: &[String],
        runtime: &identity::Did,
    ) -> Result<()> {
        fence_arrivals(txn, "PeerPairingApplied").await?;
        let response = txn
            .execute("{ PeerPairingApplied { _docID peer_id node_did } }")
            .await?;
        let mut ids = Vec::new();
        for row in
            gents_protocol::graphql::graphql_rows_from_response(&response, "PeerPairingApplied")
        {
            let id = row["_docID"]
                .as_str()
                .context("applied pairing has no document ID")?;
            let in_partition = row["peer_id"].as_str() == Some(peer_id)
                && row["node_did"].as_str() == Some(runtime.as_str());
            if selected.iter().any(|selected| selected == id) {
                anyhow::ensure!(
                    in_partition,
                    "selected applied pairing belongs to another partition"
                );
            }
            if in_partition {
                ids.push(id.to_owned());
            }
        }
        if !ids.is_empty() {
            let owners = txn.document_owners("PeerPairingApplied", &ids).await?;
            anyhow::ensure!(
                owners.len() == ids.len()
                    && owners
                        .iter()
                        .all(|owner| owner.visible && owner.owner.as_ref() == Some(runtime)),
                "applied pairing has another native owner"
            );
        }
        Ok(())
    }
    /// Construct a store for explicit `PeerPairingDesired` ownership only;
    /// runtime data-plane materialization must use
    /// [`Self::with_enrollment_authority`] so it cannot bypass the durable
    /// enrollment projection.
    pub fn for_explicit_desired(node: Arc<EmbeddedNode>, identity: Arc<dyn NodeIdentity>) -> Self {
        Self {
            node,
            identity,
            enrollment: None,
            exact_enrollment: None,
        }
    }

    pub fn with_enrollment_authority(
        node: Arc<EmbeddedNode>,
        identity: Arc<dyn NodeIdentity>,
        enrollment: EnrollmentAuthorityHandle,
    ) -> Self {
        Self {
            node,
            identity,
            enrollment: Some(enrollment),
            exact_enrollment: None,
        }
    }

    /// Fenced by one already-verified exact enrollment generation; the desired
    /// document is evidence only.
    pub fn for_enrollment_materialization(
        node: Arc<EmbeddedNode>,
        identity: Arc<dyn NodeIdentity>,
        entry: EnrollmentEndpointEntry,
    ) -> Self {
        Self {
            node,
            identity,
            enrollment: None,
            exact_enrollment: Some(entry),
        }
    }

    async fn data_plane_materialized_entry(
        &self,
        peer_id: &str,
    ) -> Result<Option<(MaterializedDataPlaneEntry, EnrollmentRouteGeneration)>> {
        let now = Utc::now();
        let (entry, generation) = if let Some(enrollment) = self.enrollment.as_ref() {
            let fence = match enrollment.fresh_peer_authorization(peer_id).await {
                Ok(Some(fence)) => fence,
                Ok(None) => return Ok(None),
                Err(error) => {
                    tracing::warn!(target: "gents::runtime::agent::p2p_reconcile::engine",
                        peer_id,
                        error = %error,
                        "enrollment authority projection unavailable; closing materialized route"
                    );
                    return Ok(None);
                }
            };
            let generation = EnrollmentRouteGeneration::from(&fence);
            let entry = EnrollmentEndpointEntry {
                desired_id: fence.member_peer.clone(),
                peer_id: fence.member_peer,
                node_did: fence.member_did,
                address: fence.member_ticket,
                request_digest: fence.request_digest,
                authorization_sequence: fence.authorization_sequence,
                authorization_expires_at: fence.authorization_expires_at,
            };
            (entry, generation)
        } else if let Some(exact) = self.exact_enrollment.as_ref() {
            let generation = EnrollmentRouteGeneration {
                member_did: exact.node_did.clone(),
                member_peer: exact.peer_id.clone(),
                member_ticket: exact.address.clone(),
                request_digest: exact.request_digest.clone(),
                authorization_sequence: exact.authorization_sequence,
                authorization_expires_at: exact.authorization_expires_at.clone(),
            };
            (exact.clone(), generation)
        } else {
            return Ok(None);
        };
        if !enrollment_entry_is_fresh_at(&entry, now) {
            return Ok(None);
        }
        Ok(
            materialized_enrollment_entry(&[entry], peer_id, self.identity.did())
                .map(|entry| (entry, generation)),
        )
    }

    /// A merge that brings in a second row for an existing `peer_id` keeps only
    /// the smallest docID in the unique index; the other row is reachable by
    /// scan but not by a `peer_id` filter, so twins are found by scanning the
    /// collection once for the whole sweep.
    async fn load_applied_rows(&self) -> Result<Vec<AppliedStateRow>> {
        let query = format!(
            r#"{{
            PeerPairingApplied(filter: {{node_did: {{_eq: "{}"}}}}) {{
                _docID
                peer_id
                collections
                replicator_addresses
                replicator_filter
                enrollment_request_digest
                enrollment_authorization_sequence
                enrollment_authorization_expires_at
            }}
        }}"#,
            escape_graphql_string(self.identity.did())
        );
        let runtime = identity::Did::new(self.identity.did())?;
        self.runtime_access()?
            .transact_readonly("p2p.load_applied_owned", |txn| {
                let query = &query;
                let runtime = &runtime;
                Box::pin(async move {
                    let response = txn.execute_local_response(query).await?;
                    let found = rows::<AppliedStateRow>(&response, "PeerPairingApplied")?;
                    let ids = found
                        .iter()
                        .map(|row| row.doc_id.clone())
                        .collect::<Vec<_>>();
                    if ids.is_empty() {
                        return Ok::<_, anyhow::Error>(found);
                    }
                    let owners = txn.document_owners("PeerPairingApplied", &ids).await?;
                    anyhow::ensure!(
                        owners.len() == ids.len(),
                        "applied pairing owner lookup is incomplete"
                    );
                    Ok::<_, anyhow::Error>(
                        found
                            .into_iter()
                            .filter(|row| {
                                owners.iter().any(|owner| {
                                    owner.doc_id == row.doc_id
                                        && owner.visible
                                        && owner.owner.as_ref() == Some(runtime)
                                })
                            })
                            .collect(),
                    )
                })
            })
            .await
    }
}

#[async_trait]
impl PairingStateStore for GraphqlPairingStateStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(self.load_desired_with_authority(peer_id).await?.state)
    }

    async fn load_desired_with_authority(&self, peer_id: &str) -> Result<LoadedPairingDesired> {
        let raw_peer_id = peer_id.to_string();
        let peer_id = escape_graphql_string(peer_id);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    _docID
                    actor_did
                    peer_id
                    node_did
                    replicator_addresses
                    template
                    source
                    enrollment_request_digest
                    enrollment_authorization_sequence
                    enrollment_authorization_expires_at
                }}
                DataPlanePairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                    _docID
                    actor_did
                    peer_id
                    node_did
                    collections
                    replicator_addresses
                    template
                    source
                }}
            }}"#
        );
        let runtime = identity::Did::new(self.identity.did())?;
        let response =
            self.runtime_access()?
                .transact_readonly("p2p.query_pairing_desired", |txn| {
                    let query = &query;
                    let runtime = &runtime;
                    Box::pin(async move {
                        let response = txn.execute_local_response(query).await?;
                        let ids = rows::<serde_json::Value>(&response, "PeerPairingDesired")?
                            .into_iter()
                            .map(|row| {
                                anyhow::ensure!(
                                    row["actor_did"].as_str() == Some(runtime.as_str()),
                                    "desired pairing has another source actor"
                                );
                                row["_docID"]
                                    .as_str()
                                    .map(str::to_owned)
                                    .context("desired pairing has no document ID")
                            })
                            .collect::<Result<Vec<_>>>()?;
                        if !ids.is_empty() {
                            let owners = txn.document_owners("PeerPairingDesired", &ids).await?;
                            anyhow::ensure!(
                                owners.len() == ids.len()
                                    && owners.iter().all(|owner| owner.visible
                                        && owner.owner.as_ref() == Some(runtime)),
                                "desired pairing has another native owner"
                            );
                        }
                        let overlays =
                            rows::<serde_json::Value>(&response, "DataPlanePairingDesired")?;
                        let mut overlay_ids = Vec::new();
                        let mut overlay_actors = Vec::new();
                        for row in overlays {
                            let doc = row["_docID"]
                                .as_str()
                                .context("pairing overlay has no document ID")?
                                .to_owned();
                            let actor = identity::Did::new(
                                row["actor_did"]
                                    .as_str()
                                    .context("pairing overlay has no author")?,
                            )?;
                            anyhow::ensure!(
                                &actor != runtime,
                                "pairing overlay uses runtime authority"
                            );
                            overlay_ids.push(doc);
                            overlay_actors.push(actor);
                        }
                        if !overlay_ids.is_empty() {
                            let owners = txn
                                .document_owners("DataPlanePairingDesired", &overlay_ids)
                                .await?;
                            anyhow::ensure!(
                                owners.len() == overlay_ids.len()
                                    && overlay_ids.iter().zip(&overlay_actors).all(
                                        |(id, actor)| owners
                                            .iter()
                                            .any(|owner| owner.doc_id == *id
                                                && owner.visible
                                                && owner.owner.as_ref() == Some(actor))
                                    ),
                                "pairing overlay has another native owner"
                            );
                        }
                        Ok::<_, anyhow::Error>(response)
                    })
                })
                .await?;
        let materialized_entry = self
            .data_plane_materialized_entry(&raw_peer_id)
            .await
            .with_context(|| format!("checking enrollment authority for {raw_peer_id}"))?;
        let Some((entry, generation)) = materialized_entry else {
            return Ok(LoadedPairingDesired::default());
        };
        let base = first_row::<PairingStateRow>(&response, "PeerPairingDesired")?
            .and_then(|row| enrollment_base_row(row, &entry, self.identity.did()))
            .map(|row| {
                data_plane_desired_from_pairing_row(row, &entry.endpoint, self.identity.did())
            })
            .transpose()?
            .flatten();
        let data_plane = first_row::<PairingStateRow>(&response, "DataPlanePairingDesired")?
            .and_then(|row| local_data_plane_row(row, &entry, self.identity.did()))
            .map(|row| {
                data_plane_desired_from_pairing_row(row, &entry.endpoint, self.identity.did())
            })
            .transpose()?
            .flatten();
        Ok(LoadedPairingDesired {
            state: merge_layered_desired(
                self.identity.did(),
                &entry.endpoint.node_did,
                base,
                data_plane,
            ),
            enrollment_generation: Some(generation),
        })
    }

    async fn enrollment_generation_is_current(
        &self,
        generation: &EnrollmentRouteGeneration,
    ) -> Result<bool> {
        if let Some(enrollment) = self.enrollment.as_ref() {
            return Ok(enrollment
                .fresh_authorization(&generation.member_did, &generation.member_peer)
                .await?
                .as_ref()
                .map(EnrollmentRouteGeneration::from)
                .as_ref()
                == Some(generation));
        }
        Ok(self.exact_enrollment.as_ref().is_some_and(|entry| {
            enrollment_entry_is_fresh_at(entry, Utc::now())
                && entry.node_did == generation.member_did
                && entry.peer_id == generation.member_peer
                && entry.address == generation.member_ticket
                && entry.request_digest == generation.request_digest
                && entry.authorization_sequence == generation.authorization_sequence
                && entry.authorization_expires_at == generation.authorization_expires_at
        }))
    }

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(self
            .load_applied_many(&BTreeSet::from([peer_id.to_owned()]))
            .await?
            .remove(peer_id)
            .unwrap_or_default())
    }

    async fn load_applied_many(
        &self,
        peer_ids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, LoadedPairingApplied>> {
        if peer_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut grouped = BTreeMap::<String, Vec<AppliedStateRow>>::new();
        for row in self.load_applied_rows().await? {
            if peer_ids.contains(&row.peer_id) {
                grouped.entry(row.peer_id.clone()).or_default().push(row);
            }
        }
        Ok(grouped
            .into_iter()
            .map(|(peer_id, mut rows)| {
                rows.sort_by(|left, right| left.doc_id.cmp(&right.doc_id));
                let mut rows = rows.into_iter();
                let canonical = rows.next().expect("only populated groups are retained");
                (
                    peer_id,
                    LoadedPairingApplied {
                        doc_id: Some(canonical.doc_id.clone()),
                        enrollment: canonical.enrollment.clone(),
                        state: canonical.into_applied(),
                        duplicate_doc_ids: rows.map(|row| row.doc_id).collect(),
                    },
                )
            })
            .collect())
    }

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        let access = self.runtime_access()?;
        let runtime = identity::Did::new(self.identity.did())?;
        let raw_peer_id = peer_id.to_owned();
        let selected = applied
            .doc_id
            .iter()
            .chain(&applied.duplicate_doc_ids)
            .cloned()
            .collect::<Vec<_>>();
        if applied.state.is_empty() {
            let delete_observed =
                if applied.doc_id.is_none() && applied.duplicate_doc_ids.is_empty() {
                    String::new()
                } else {
                    let ids = graphql_string_list_literal(
                        applied
                            .doc_id
                            .iter()
                            .chain(&applied.duplicate_doc_ids)
                            .map(String::as_str),
                    );
                    format!("observed: delete_PeerPairingApplied(docIDs: {ids}) {{ _docID }}")
                };
            let peer_id = escape_graphql_string(peer_id);
            let mutation = format!(
                r#"mutation {{ {delete_observed}
                    current: delete_PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }}, node_did: {{_eq: "{}"}} }}) {{ _docID }}
                }}"#,
                escape_graphql_string(self.identity.did())
            );
            return access
                .transact_write("p2p.delete_pairing_applied", |txn| {
                    let mutation = &mutation;
                    let runtime = &runtime;
                    let selected = &selected;
                    let peer_id = &raw_peer_id;
                    Box::pin(async move {
                        txn.validate_runtime_actor(runtime)?;
                        Self::verify_applied_owners(txn, peer_id, selected, runtime).await?;
                        txn.execute(mutation).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await;
        }

        let peer_id = escape_graphql_string(peer_id);
        let collections = graphql_nullable_string_array(&applied.state.collections);
        let replicator_addresses =
            graphql_nullable_string_array(&applied.state.replicator_addresses);
        let replicator_filter = graphql_nullable_filter_literal(&applied.state.replicator_filter);
        let digest = graphql_nullable_string_literal(applied.enrollment.request_digest.as_deref());
        let sequence = applied
            .enrollment
            .authorization_sequence
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".into());
        let expires_at =
            graphql_nullable_string_literal(applied.enrollment.authorization_expires_at.as_deref());
        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let save = format!(
            r#"upsert_PeerPairingApplied(
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }}, node_did: {{_eq: "{}"}} }},
                    add: {{
                        peer_id: "{peer_id}",
                        node_did: "{}",
                        parent_linked: false,
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        replicator_filter: {replicator_filter},
                        enrollment_request_digest: {digest},
                        enrollment_authorization_sequence: {sequence},
                        enrollment_authorization_expires_at: {expires_at},
                        created_at: "{now}",
                        updated_at: "{now}"
                    }},
                    update: {{
                        collections: {collections},
                        replicator_addresses: {replicator_addresses},
                        replicator_filter: {replicator_filter},
                        enrollment_request_digest: {digest},
                        enrollment_authorization_sequence: {sequence},
                        enrollment_authorization_expires_at: {expires_at},
                        updated_at: "{now}"
                    }}
                ) {{ _docID }}"#,
            escape_graphql_string(self.identity.did()),
            escape_graphql_string(self.identity.did()),
        );
        let delete_duplicates = if applied.duplicate_doc_ids.is_empty() {
            String::new()
        } else {
            let ids =
                graphql_string_list_literal(applied.duplicate_doc_ids.iter().map(String::as_str));
            format!(
                r#"delete_PeerPairingApplied(
                    docIDs: {ids}
                ) {{ _docID }}"#
            )
        };
        let mutation = format!("mutation {{ {delete_duplicates} {save} }}");
        access
            .transact_write("p2p.save_pairing_applied", |txn| {
                let mutation = &mutation;
                let runtime = &runtime;
                let selected = &selected;
                let peer_id = &raw_peer_id;
                Box::pin(async move {
                    txn.validate_runtime_actor(runtime)?;
                    Self::verify_applied_owners(txn, peer_id, selected, runtime).await?;
                    let response = txn.execute(mutation).await?;
                    let doc = gents_protocol::graphql::extract_mutation_doc_id(
                        &response,
                        "PeerPairingApplied",
                    )?;
                    crate::store::config_client::maybe_link_deferred_operation_in_txn(
                        txn,
                        "PeerPairingApplied",
                        &doc,
                        runtime,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            PeerPairingDesired { peer_id }
            DataPlanePairingDesired { peer_id }
        }"#;
        let response = self.runtime_query(query).await?;
        let mut ids = BTreeSet::new();
        for row in rows::<PeerIdRow>(&response, "PeerPairingDesired")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        for row in rows::<PeerIdRow>(&response, "DataPlanePairingDesired")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        for row in self.load_applied_rows().await? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        Ok(ids)
    }
}
