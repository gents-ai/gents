use serde::{Deserialize, Serialize};

use crate::p2p_reconcile::engine::desired::DataPlaneRefusal;
use crate::p2p_reconcile::engine::enrollment::{
    enrollment_base_row, local_data_plane_row, materialized_enrollment_entry,
};
use crate::p2p_reconcile::engine::{
    data_plane_desired_from_pairing_row, merge_layered_desired, EnrollmentEndpointEntry,
    PairingStateRow,
};
use crate::p2p_reconcile::{equality_filter, single_string_eq, FilterPredicate, PairingDesired};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Refusal {
    ForeignScope,
    MissingTemplate,
    UnknownTemplate,
    MissingRouteKey,
    UndirectedRouteKey,
    UnsignedPeer,
}

macro_rules! row_codec {
    ($family:ident) => {
        fn stored(row: &$family::Row) -> PairingStateRow {
            PairingStateRow {
                peer_id: row.route_key.clone(),
                node_did: row.node_did.clone(),
                replicator_addresses: (!row.addresses.is_empty()).then(|| row.addresses.clone()),
                template: row.template.clone(),
                source: row.source.clone(),
                enrollment_request_digest: row.request_digest.clone(),
                enrollment_authorization_sequence: row.sequence.map(|sequence| sequence as i64),
                enrollment_authorization_expires_at: row.expires_at.clone(),
            }
        }

        fn signed(endpoint: &$family::Endpoint) -> EnrollmentEndpointEntry {
            EnrollmentEndpointEntry {
                desired_id: endpoint.desired_id.clone(),
                peer_id: endpoint.peer_id.clone(),
                node_did: endpoint.node_did.clone(),
                address: endpoint.address.clone(),
                request_digest: endpoint.request_digest.clone(),
                authorization_sequence: endpoint.sequence,
                authorization_expires_at: endpoint.expires_at.clone(),
            }
        }
    };
}

macro_rules! desired_codec {
    ($family:ident) => {
        fn modeled(desired: PairingDesired) -> $family::Desired {
            $family::Desired {
                addresses: desired.replicator_addresses.into_iter().collect(),
                collections: desired.replicator_collections.into_iter().collect(),
                filter: desired
                    .replicator_filter
                    .iter()
                    .map(|(collection, filter)| {
                        let (field, value) =
                            single_string_eq(filter).expect("a route scope is one equality");
                        $family::Scope {
                            collection: collection.clone(),
                            field: field.to_owned(),
                            value: value.to_owned(),
                        }
                    })
                    .collect(),
                templates: desired.template_ids.into_iter().collect(),
            }
        }
    };
}

#[test]
fn data_plane_desired_matches_lean() {
    use gents_lean_vocab::families::pairing_data_plane_desired as family;
    row_codec!(family);
    desired_codec!(family);

    family::replay::<Refusal>(|input| {
        match data_plane_desired_from_pairing_row(
            stored(&input.row),
            &signed(&input.endpoint),
            &input.self_did,
        ) {
            Ok(desired) => family::DesiredOutput {
                refusal: None,
                desired: Some(modeled(desired)),
            },
            Err(refusal) => family::DesiredOutput {
                refusal: Some(match refusal {
                    DataPlaneRefusal::ForeignScope { .. } => Refusal::ForeignScope,
                    DataPlaneRefusal::MissingTemplate => Refusal::MissingTemplate,
                    DataPlaneRefusal::UnknownTemplate(_) => Refusal::UnknownTemplate,
                    DataPlaneRefusal::MissingRouteKey => Refusal::MissingRouteKey,
                    DataPlaneRefusal::UndirectedRouteKey(_) => Refusal::UndirectedRouteKey,
                    DataPlaneRefusal::UnsignedPeer { .. } => Refusal::UnsignedPeer,
                }),
                desired: None,
            },
        }
    });
}

#[test]
fn pairing_row_ownership_matches_lean() {
    use gents_lean_vocab::families::pairing_data_plane_ownership as family;
    row_codec!(family);

    fn modeled(row: PairingStateRow) -> family::Row {
        family::Row {
            route_key: row.peer_id,
            node_did: row.node_did,
            addresses: row.replicator_addresses.unwrap_or_default(),
            template: row.template,
            source: row.source,
            request_digest: row.enrollment_request_digest,
            sequence: row
                .enrollment_authorization_sequence
                .map(|sequence| sequence as u64),
            expires_at: row.enrollment_authorization_expires_at,
        }
    }

    family::replay(|input| {
        let entries: Vec<_> = input.entries.iter().map(signed).collect();
        let entry = materialized_enrollment_entry(&entries, &input.key, &input.self_did);
        family::OwnershipOutput {
            admitted: entry.as_ref().map(|entry| family::Endpoint {
                desired_id: entry.desired_id.clone(),
                peer_id: entry.peer_id.clone(),
                node_did: entry.node_did.clone(),
                address: entry.address.clone(),
                request_digest: entry.request_digest.clone(),
                sequence: entry.authorization_sequence,
                expires_at: entry.authorization_expires_at.clone(),
            }),
            base: entry
                .as_ref()
                .and_then(|entry| enrollment_base_row(stored(&input.row), entry, &input.self_did))
                .map(modeled),
            overlay: entry
                .as_ref()
                .and_then(|entry| local_data_plane_row(stored(&input.row), entry, &input.self_did))
                .map(modeled),
        }
    });
}

#[test]
fn layered_desired_merge_matches_lean() {
    use gents_lean_vocab::families::pairing_data_plane_merge as family;
    desired_codec!(family);

    fn layer(desired: &family::Desired) -> PairingDesired {
        PairingDesired {
            replicator_addresses: desired.addresses.iter().cloned().collect(),
            replicator_collections: desired.collections.iter().cloned().collect(),
            replicator_filter: desired
                .filter
                .iter()
                .map(|scope| {
                    (
                        scope.collection.clone(),
                        equality_filter(&scope.field, &scope.value),
                    )
                })
                .collect(),
            template_ids: desired.templates.iter().cloned().collect(),
        }
    }

    family::replay(|input| family::MergeOutput {
        merged: merge_layered_desired(
            &input.local_did,
            &input.peer_did,
            input.base.as_ref().map(layer),
            input.data_plane.as_ref().map(layer),
        )
        .map(modeled),
    });
}

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn merge_desired(
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    merge_layered_desired("did:key:local", "did:key:peer", base, data_plane)
}

#[test]
fn layered_desired_merge_prefers_signed_data_plane_filter() {
    let base_filter = equality_filter("requester_did", "did:key:phone");
    let data_filter = FilterPredicate::predicate(
        serde_json::json!({ "lifecycle_state": { "_in": ["pending", "processing"] } })
            .as_object()
            .expect("object")
            .clone(),
    );
    let layer = |filter| PairingDesired {
        replicator_addresses: set(&["addr"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: [("AgentRequest".to_string(), filter)].into_iter().collect(),
        template_ids: BTreeSet::new(),
    };

    let merged = merge_desired(
        Some(layer(base_filter.clone())),
        Some(layer(data_filter.clone())),
    )
    .expect("merged desired state");

    assert_eq!(
        merged.replicator_filter.get("AgentRequest"),
        Some(&data_filter)
    );
}

/// Lean `base_preserved` and `data_plane_filters_preserved`: merging keeps
/// the control layer's replication and adds the data-plane layer's.
#[test]
fn data_plane_layer_adds_replication_beside_control_pairing() {
    let base = PairingDesired {
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["ControlA", "ControlB"]),
        replicator_filter: Default::default(),
        template_ids: set(&["machine"]),
    };
    let data_plane = PairingDesired {
        replicator_addresses: set(&["addr-b"]),
        replicator_collections: set(&["ChangeProposed"]),
        replicator_filter: Default::default(),
        template_ids: set(&["client"]),
    };
    let merged = merge_desired(Some(base.clone()), Some(data_plane)).expect("merged");
    assert!(merged.replicator_collections.contains("ControlA"));
    assert!(merged.replicator_collections.contains("ChangeProposed"));
    assert!(
        merged.replicator_filter.is_empty(),
        "both layers unscoped => no filter"
    );
    assert!(merged.template_ids.contains("machine"));
}
