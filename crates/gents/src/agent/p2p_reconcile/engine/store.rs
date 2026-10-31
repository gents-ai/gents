use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use defra_node::EmbeddedNode;

use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
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
                    tracing::warn!(target: "gents::agent::p2p_reconcile::engine",
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
        let query = r#"{
            PeerPairingApplied {
                _docID
                peer_id
                collections
                replicator_addresses
                replicator_filter
                enrollment_request_digest
                enrollment_authorization_sequence
                enrollment_authorization_expires_at
            }
        }"#;
        let response =
            graphql_with_transaction_retry(&self.node, query, "query PeerPairingApplied").await?;
        Ok(rows::<AppliedStateRow>(&response, "PeerPairingApplied")?)
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
                    peer_id
                    node_did
                    collections
                    replicator_addresses
                    template
                    source
                }}
            }}"#
        );
        let response =
            graphql_with_transaction_retry(&self.node, &query, "query pairing desired state")
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
                    current: delete_PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{ _docID }}
                }}"#
            );
            return Ok(
                crate::store::config_client::ConfigAccess::write_local_response(
                    &self.node,
                    "p2p.delete_pairing_applied",
                    &mutation,
                )
                .await
                .map(|_| ())?,
            );
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
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                    add: {{
                        peer_id: "{peer_id}",
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
        Ok(
            crate::store::config_client::ConfigAccess::write_local_response(
                &self.node,
                "p2p.save_pairing_applied",
                &mutation,
            )
            .await
            .map(|_| ())?,
        )
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        let query = r#"{
            PeerPairingDesired { peer_id }
            DataPlanePairingDesired { peer_id }
            PeerPairingApplied { peer_id }
        }"#;
        let response =
            graphql_with_transaction_retry(&self.node, query, "query pairing peer ids").await?;
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
        for row in rows::<PeerIdRow>(&response, "PeerPairingApplied")? {
            if !row.peer_id.trim().is_empty() {
                ids.insert(row.peer_id);
            }
        }
        Ok(ids)
    }
}
