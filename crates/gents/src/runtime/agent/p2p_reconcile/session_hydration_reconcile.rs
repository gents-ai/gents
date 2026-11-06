use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::Arc;

use anyhow::{Context, Result};
#[cfg(test)]
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;

#[cfg(test)]
use super::session_hydration::HydrationRequest;
use super::session_hydration::{hydration_collection_name, HydrationDocument};
use super::session_hydration_closure::{
    build_canonical_selection, CanonicalClosureInput, CanonicalPublicationOrigin, ScopedDocument,
    ToolEffectRow,
};
use crate::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use crate::store::graphql::escape_graphql_string;

mod inventory;
mod inventory_order;
mod selection;
pub use inventory::{FileInventoryPageError, FileInventorySnapshot};

#[derive(Debug, Clone, Default)]
struct HydrationSelection {
    documents: BTreeSet<HydrationDocument>,
    publications: Vec<CanonicalPublicationOrigin>,
    headers: Vec<crate::session::canonical_rows::TranscriptMessageRow>,
    output_segments: Vec<crate::session::canonical_rows::OutputSegmentRow>,
}

#[derive(Deserialize)]
struct TranscriptRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
    requester_did: Option<String>,
    node_did: Option<String>,
    session_id: Option<String>,
}
