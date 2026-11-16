//! Native file inventory of one session: its canonical closure, the accepted
//! tools that closure selects, and their metadata projections.

use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::Arc;

use anyhow::{Context, Result};
#[cfg(test)]
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;

use closure::{
    build_canonical_selection, CanonicalClosureInput, CanonicalPublicationOrigin, ScopedDocument,
    ToolEffectRow,
};
#[cfg(test)]
use gents_protocol::file_inventory::FileInventoryRequest;
use gents_protocol::output::rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use gents_store::graphql::escape_graphql_string;

pub mod admission;
mod closure;
mod direct;
mod order;
mod selection;
mod snapshot;

pub use admission::{session_collection_name, SessionCollection, SessionDocument};
pub use direct::read_direct_inventory;
pub use snapshot::{FileInventoryPageError, FileInventorySnapshot};

#[derive(Debug, Clone, Default)]
struct SessionSelection {
    documents: BTreeSet<SessionDocument>,
    publications: Vec<CanonicalPublicationOrigin>,
    headers: Vec<gents_protocol::output::rows::TranscriptMessageRow>,
    output_segments: Vec<gents_protocol::output::rows::OutputSegmentRow>,
}

#[derive(Deserialize)]
struct TranscriptRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
    requester_did: Option<String>,
    node_did: Option<String>,
    session_id: Option<String>,
}

#[cfg(test)]
mod tests;
