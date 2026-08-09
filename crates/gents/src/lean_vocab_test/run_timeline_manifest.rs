use serde::Deserialize;

/// Lean-generated frozen run-timeline source-manifest witness.
#[derive(Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRunTimelineManifestCase {
    pub(crate) name: String,
    pub(crate) disposition: String,
    pub(crate) selector: String,
    pub(crate) visible_logical_roots: usize,
    pub(crate) root_doc_id: Option<usize>,
    pub(crate) root_cid: Option<usize>,
    pub(crate) expected_slots: usize,
    pub(crate) included_slots: usize,
    pub(crate) omitted_slots: usize,
    pub(crate) ordered_source_classes: Vec<String>,
    pub(crate) ordered_collections: Vec<usize>,
    pub(crate) ordered_collection_version_ids: Vec<usize>,
    pub(crate) ordered_doc_ids: Vec<usize>,
    pub(crate) ordered_cids: Vec<usize>,
    pub(crate) exact_membership: bool,
    pub(crate) complete_coverage: bool,
    pub(crate) canonical_order: bool,
    pub(crate) manifest_version: Option<usize>,
    pub(crate) manifest_status: Option<String>,
    pub(crate) coverage_gap_count: usize,
    pub(crate) ordered_coverage_gap_kinds: Vec<String>,
    pub(crate) canonical_gaps: bool,
}
