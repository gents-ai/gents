use std::collections::BTreeSet;

use serde::Deserialize;

use crate::store::graphql::escape_graphql_string;

use super::super::graphql_helpers::graphql_string_list_literal;
use super::super::templates::{decode_pairing_filters, PairingFilters};
use super::super::PairingApplied;

#[derive(Default, Deserialize)]
pub(super) struct PairingStateRow {
    #[serde(default)]
    pub(super) peer_id: Option<String>,
    #[serde(default)]
    pub(super) node_did: Option<String>,
    pub(super) collections: Option<Vec<String>>,
    pub(super) replicator_addresses: Option<Vec<String>>,
    #[serde(default)]
    pub(super) template: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) enrollment_request_digest: Option<String>,
    #[serde(default)]
    pub(super) enrollment_authorization_sequence: Option<i64>,
    #[serde(default)]
    pub(super) enrollment_authorization_expires_at: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AppliedStateRow {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    #[serde(default)]
    pub(super) peer_id: String,
    collections: Option<Vec<String>>,
    pub(super) replicator_addresses: Option<Vec<String>>,
    #[serde(default)]
    replicator_filter: Option<String>,
    #[serde(flatten)]
    pub(super) enrollment: super::EnrollmentMaterialization,
}

impl AppliedStateRow {
    pub(super) fn into_applied(self) -> PairingApplied {
        PairingApplied {
            collections: self.collections.unwrap_or_default().into_iter().collect(),
            replicator_addresses: self
                .replicator_addresses
                .unwrap_or_default()
                .into_iter()
                .collect(),
            replicator_filter: decode_replicator_filter(self.replicator_filter.as_deref()),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct PeerIdRow {
    pub(super) peer_id: String,
}

pub(super) fn graphql_nullable_filter_literal(filter: &PairingFilters) -> String {
    if filter.is_empty() {
        return "null".to_string();
    }
    let json = serde_json::to_string(filter).unwrap_or_default();
    format!(r#""{}""#, escape_graphql_string(&json))
}

fn decode_replicator_filter(value: Option<&str>) -> PairingFilters {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return PairingFilters::default();
    };
    decode_pairing_filters(raw).unwrap_or_else(|error| {
        tracing::warn!(target: "gents::runtime::agent::p2p_reconcile::engine",
            error = %error,
            "PeerPairingApplied.replicator_filter failed to decode; treating as unfiltered"
        );
        PairingFilters::default()
    })
}

pub(super) fn graphql_nullable_string_array(values: &BTreeSet<String>) -> String {
    graphql_string_list_literal(values.iter().map(String::as_str))
}
