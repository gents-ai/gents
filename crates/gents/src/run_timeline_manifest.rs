//! Exact source boundary for run timelines and adapter projections.
//!
//! Logical identifiers are discovery keys only. A source manifest selects one
//! exact signed request root and contains one include/omit decision for every
//! canonical observed source slot, plus explicit gaps for any open domain.
//! Presentation ordering remains the concern of
//! [`crate::run_timeline`]; this module orders evidence by source policy.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{DocumentVersionRef, SignedDocumentVersionRef};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedDocumentVersionRef {
    doc_id: String,
    composite_commit_cid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedSignedDocumentVersionRef {
    version: SerializedDocumentVersionRef,
    signer_did: String,
}

fn deserialize_strict_signed_document_version<'de, D>(
    deserializer: D,
) -> Result<SignedDocumentVersionRef, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let serialized = SerializedSignedDocumentVersionRef::deserialize(deserializer)?;
    Ok(SignedDocumentVersionRef {
        version: DocumentVersionRef {
            doc_id: serialized.version.doc_id,
            composite_commit_cid: serialized.version.composite_commit_cid,
        },
        signer_did: serialized.signer_did,
    })
}

pub const RUN_TIMELINE_MANIFEST_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineSourceClass {
    Request,
    SessionProjection,
    ConversationProjection,
    Message,
    ToolCall,
    ToolResult,
    ToolOutputOmission,
    ToolApproval,
    ResponseLive,
    ResponseOutcome,
    InferenceCall,
    RenderedRequest,
    ResolvedConfig,
    Compaction,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineSourceSlot {
    pub source_class: TimelineSourceClass,
    pub ordinal: u32,
}

impl TimelineSourceSlot {
    pub const fn new(source_class: TimelineSourceClass, ordinal: u32) -> Self {
        Self {
            source_class,
            ordinal,
        }
    }

    pub const fn root() -> Self {
        Self::new(TimelineSourceClass::Request, 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRootCandidate {
    pub request_id: String,
    pub exact: SignedDocumentVersionRef,
    pub current_head_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineRootSelector {
    Exact(SignedDocumentVersionRef),
    LogicalRequestId(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineOmissionReason {
    NotProduced,
    NotApplicable,
    ProjectionExcluded,
    Redacted,
    LegacyUnavailable,
    Denied,
    Erased,
    UnsupportedManifest,
    HeuristicLogicalJoin,
    RemoteSignatureUnverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineCoverageGapKind {
    OpenLogicalExtent,
    OpenSessionExtent,
    NonAtomicObservation,
    RemoteSignatureUnverified,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineCoverageGap {
    pub kind: TimelineCoverageGapKind,
    pub source_class: TimelineSourceClass,
    pub collection: String,
    pub scope_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineManifestStatus {
    VerifiedExact,
    PartialExact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineSlotRequirement {
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineExpectedSlot {
    pub slot: TimelineSourceSlot,
    pub requirement: TimelineSlotRequirement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineObservedSource {
    pub slot: TimelineSourceSlot,
    pub collection: String,
    pub collection_version_id: String,
    pub exact: SignedDocumentVersionRef,
}

/// An exact reference declared inside another persisted timeline fact.
///
/// The outer fact is not closed under provenance until this collection/schema/
/// document/commit/signer tuple is also present as an included manifest source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineDeclaredExactEdge {
    pub collection: String,
    pub collection_version_id: String,
    pub exact: SignedDocumentVersionRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineSourceDecision {
    Include {
        slot: TimelineSourceSlot,
        collection: String,
        collection_version_id: String,
        exact: SignedDocumentVersionRef,
    },
    Omit {
        slot: TimelineSourceSlot,
        collection: String,
        reason: TimelineOmissionReason,
    },
}

impl TimelineSourceDecision {
    pub fn slot(&self) -> &TimelineSourceSlot {
        match self {
            Self::Include { slot, .. } | Self::Omit { slot, .. } => slot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineManifestSource {
    pub slot: TimelineSourceSlot,
    pub collection: String,
    pub collection_version_id: String,
    #[serde(deserialize_with = "deserialize_strict_signed_document_version")]
    pub exact: SignedDocumentVersionRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineManifestOmission {
    pub slot: TimelineSourceSlot,
    pub collection: String,
    pub reason: TimelineOmissionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunTimelineSourceManifest {
    pub manifest_version: u32,
    pub status: TimelineManifestStatus,
    pub root: SignedDocumentVersionRef,
    pub sources: Vec<TimelineManifestSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omissions: Vec<TimelineManifestOmission>,
    pub coverage_gaps: Vec<TimelineCoverageGap>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedRunTimelineSourceManifest {
    manifest_version: u32,
    status: TimelineManifestStatus,
    #[serde(deserialize_with = "deserialize_strict_signed_document_version")]
    root: SignedDocumentVersionRef,
    sources: Vec<TimelineManifestSource>,
    #[serde(default)]
    omissions: Vec<TimelineManifestOmission>,
    coverage_gaps: Vec<TimelineCoverageGap>,
}

impl<'de> Deserialize<'de> for RunTimelineSourceManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let serialized = SerializedRunTimelineSourceManifest::deserialize(deserializer)?;
        let manifest = Self {
            manifest_version: serialized.manifest_version,
            status: serialized.status,
            root: serialized.root,
            sources: serialized.sources,
            omissions: serialized.omissions,
            coverage_gaps: serialized.coverage_gaps,
        };
        manifest.validate().map_err(serde::de::Error::custom)?;
        Ok(manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineManifestError {
    UnsupportedManifestVersion(u32),
    RootNotFound,
    AmbiguousRoot {
        matches: Vec<SignedDocumentVersionRef>,
    },
    RootHasAmbiguousHeads {
        doc_id: String,
        current_head_count: usize,
    },
    IncompleteRootEvidence {
        doc_id: String,
    },
    NonCanonicalExpectedSlots,
    DuplicateExpectedSlot(TimelineSourceSlot),
    MissingDecision(TimelineSourceSlot),
    DuplicateDecision(TimelineSourceSlot),
    UndeclaredDecision(TimelineSourceSlot),
    RequiredSourceOmitted(TimelineSourceSlot),
    MissingObservedSource(TimelineSourceSlot),
    DuplicateObservedSource(TimelineSourceSlot),
    UndeclaredObservedSource(TimelineSourceSlot),
    SourceVersionMismatch(TimelineSourceSlot),
    SourceCollectionMismatch(TimelineSourceSlot),
    SourceCollectionVersionMismatch(TimelineSourceSlot),
    IncompleteSourceEvidence(TimelineSourceSlot),
    RootDecisionMismatch,
    IncompleteCoverageGap(TimelineCoverageGap),
    DuplicateCoverageGap(TimelineCoverageGap),
    NonCanonicalCoverageGaps,
    MissingDeclaredExactEdge(TimelineDeclaredExactEdge),
    NonCanonicalManifest,
}

impl fmt::Display for TimelineManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedManifestVersion(version) => write!(
                formatter,
                "unsupported timeline source manifest version {version}"
            ),
            Self::RootNotFound => formatter.write_str("timeline root was not found"),
            Self::AmbiguousRoot { matches } => write!(
                formatter,
                "timeline root is ambiguous across exact documents {:?}",
                matches
                    .iter()
                    .map(|source| source.version.doc_id.as_str())
                    .collect::<Vec<_>>()
            ),
            Self::RootHasAmbiguousHeads {
                doc_id,
                current_head_count,
            } => write!(
                formatter,
                "timeline root {doc_id} has {current_head_count} current composite heads"
            ),
            Self::IncompleteRootEvidence { doc_id } => {
                write!(
                    formatter,
                    "timeline root {doc_id} has incomplete signed evidence"
                )
            }
            Self::NonCanonicalExpectedSlots => {
                formatter.write_str("timeline manifest expected slots are not canonically ordered")
            }
            Self::DuplicateExpectedSlot(slot) => {
                write!(
                    formatter,
                    "timeline manifest repeats expected slot {slot:?}"
                )
            }
            Self::MissingDecision(slot) => {
                write!(
                    formatter,
                    "timeline manifest has no decision for slot {slot:?}"
                )
            }
            Self::DuplicateDecision(slot) => {
                write!(
                    formatter,
                    "timeline manifest repeats decision for slot {slot:?}"
                )
            }
            Self::UndeclaredDecision(slot) => {
                write!(
                    formatter,
                    "timeline manifest decides undeclared slot {slot:?}"
                )
            }
            Self::RequiredSourceOmitted(slot) => {
                write!(formatter, "timeline manifest omits required slot {slot:?}")
            }
            Self::MissingObservedSource(slot) => {
                write!(
                    formatter,
                    "timeline manifest did not observe source slot {slot:?}"
                )
            }
            Self::DuplicateObservedSource(slot) => {
                write!(
                    formatter,
                    "timeline manifest observed source slot {slot:?} more than once"
                )
            }
            Self::UndeclaredObservedSource(slot) => {
                write!(
                    formatter,
                    "timeline manifest observed undeclared slot {slot:?}"
                )
            }
            Self::SourceVersionMismatch(slot) => {
                write!(
                    formatter,
                    "timeline source version changed for slot {slot:?}"
                )
            }
            Self::SourceCollectionMismatch(slot) => {
                write!(
                    formatter,
                    "timeline source collection changed for slot {slot:?}"
                )
            }
            Self::SourceCollectionVersionMismatch(slot) => {
                write!(
                    formatter,
                    "timeline source collection version changed for slot {slot:?}"
                )
            }
            Self::IncompleteSourceEvidence(slot) => {
                write!(
                    formatter,
                    "timeline source evidence is incomplete for slot {slot:?}"
                )
            }
            Self::RootDecisionMismatch => formatter
                .write_str("timeline root decision does not include the selected exact root"),
            Self::IncompleteCoverageGap(gap) => {
                write!(
                    formatter,
                    "timeline manifest coverage gap is incomplete {gap:?}"
                )
            }
            Self::DuplicateCoverageGap(gap) => {
                write!(formatter, "timeline manifest repeats coverage gap {gap:?}")
            }
            Self::NonCanonicalCoverageGaps => {
                formatter.write_str("timeline manifest coverage gaps are not canonically ordered")
            }
            Self::MissingDeclaredExactEdge(edge) => write!(
                formatter,
                "timeline manifest omitted declared exact edge {} schema {} {}@{} signed by {}",
                edge.collection,
                edge.collection_version_id,
                edge.exact.version.doc_id,
                edge.exact.version.composite_commit_cid,
                edge.exact.signer_did
            ),
            Self::NonCanonicalManifest => formatter.write_str(
                "timeline source manifest is not in its canonical validated representation",
            ),
        }
    }
}

impl std::error::Error for TimelineManifestError {}

fn complete_exact(source: &SignedDocumentVersionRef) -> bool {
    !source.version.doc_id.trim().is_empty()
        && !source.version.composite_commit_cid.trim().is_empty()
        && !source.signer_did.trim().is_empty()
}

pub fn resolve_timeline_root(
    selector: &TimelineRootSelector,
    candidates: &[TimelineRootCandidate],
) -> Result<SignedDocumentVersionRef, TimelineManifestError> {
    let matches = candidates
        .iter()
        .filter(|candidate| match selector {
            TimelineRootSelector::Exact(exact) => candidate.exact == *exact,
            TimelineRootSelector::LogicalRequestId(request_id) => {
                candidate.request_id == *request_id
            }
        })
        .collect::<Vec<_>>();
    let candidate = match matches.as_slice() {
        [] => return Err(TimelineManifestError::RootNotFound),
        [candidate] => *candidate,
        _ => {
            return Err(TimelineManifestError::AmbiguousRoot {
                matches: matches
                    .into_iter()
                    .map(|candidate| candidate.exact.clone())
                    .collect(),
            });
        }
    };
    if matches!(selector, TimelineRootSelector::LogicalRequestId(_))
        && candidate.current_head_count != 1
    {
        return Err(TimelineManifestError::RootHasAmbiguousHeads {
            doc_id: candidate.exact.version.doc_id.clone(),
            current_head_count: candidate.current_head_count,
        });
    }
    if !complete_exact(&candidate.exact) {
        return Err(TimelineManifestError::IncompleteRootEvidence {
            doc_id: candidate.exact.version.doc_id.clone(),
        });
    }
    Ok(candidate.exact.clone())
}

/// Freeze the degenerate manifest for a timeline whose request root is its
/// only persisted source. Non-trivial timelines must enumerate every other
/// source slot rather than extending this manifest ad hoc.
pub fn root_only_timeline_manifest(
    root: SignedDocumentVersionRef,
    collection_version_id: impl Into<String>,
) -> Result<RunTimelineSourceManifest, TimelineManifestError> {
    let collection_version_id = collection_version_id.into();
    let slot = TimelineSourceSlot::root();
    freeze_timeline_manifest(
        &TimelineRootSelector::Exact(root.clone()),
        &[TimelineRootCandidate {
            request_id: String::new(),
            exact: root.clone(),
            current_head_count: 1,
        }],
        &[TimelineExpectedSlot {
            slot: slot.clone(),
            requirement: TimelineSlotRequirement::Required,
        }],
        &[TimelineObservedSource {
            slot: slot.clone(),
            collection: "AgentRequest".to_string(),
            collection_version_id: collection_version_id.clone(),
            exact: root.clone(),
        }],
        &[TimelineSourceDecision::Include {
            slot,
            collection: "AgentRequest".to_string(),
            collection_version_id,
            exact: root,
        }],
        &[],
    )
}

pub fn freeze_timeline_manifest(
    selector: &TimelineRootSelector,
    candidates: &[TimelineRootCandidate],
    expected: &[TimelineExpectedSlot],
    observed: &[TimelineObservedSource],
    decisions: &[TimelineSourceDecision],
    coverage_gaps: &[TimelineCoverageGap],
) -> Result<RunTimelineSourceManifest, TimelineManifestError> {
    let root = resolve_timeline_root(selector, candidates)?;

    if let Some(gap) = coverage_gaps
        .iter()
        .find(|gap| gap.collection.trim().is_empty() || gap.scope_id.trim().is_empty())
    {
        return Err(TimelineManifestError::IncompleteCoverageGap(gap.clone()));
    }
    if let Some(pair) = coverage_gaps.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(TimelineManifestError::DuplicateCoverageGap(pair[0].clone()));
    }
    if coverage_gaps.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err(TimelineManifestError::NonCanonicalCoverageGaps);
    }

    let expected_slots = expected
        .iter()
        .map(|expected| expected.slot.clone())
        .collect::<Vec<_>>();
    if let Some(pair) = expected_slots.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(TimelineManifestError::DuplicateExpectedSlot(
            pair[0].clone(),
        ));
    }
    if expected_slots.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err(TimelineManifestError::NonCanonicalExpectedSlots);
    }
    let expected_set = expected_slots.iter().cloned().collect::<BTreeSet<_>>();

    let mut decisions_by_slot = BTreeMap::<TimelineSourceSlot, Vec<&TimelineSourceDecision>>::new();
    for decision in decisions {
        if !expected_set.contains(decision.slot()) {
            return Err(TimelineManifestError::UndeclaredDecision(
                decision.slot().clone(),
            ));
        }
        decisions_by_slot
            .entry(decision.slot().clone())
            .or_default()
            .push(decision);
    }
    let mut observed_by_slot = BTreeMap::<TimelineSourceSlot, Vec<&TimelineObservedSource>>::new();
    for source in observed {
        if !expected_set.contains(&source.slot) {
            return Err(TimelineManifestError::UndeclaredObservedSource(
                source.slot.clone(),
            ));
        }
        observed_by_slot
            .entry(source.slot.clone())
            .or_default()
            .push(source);
    }

    let mut sources = Vec::new();
    let mut omissions = Vec::new();
    for expected_source in expected {
        let slot = &expected_source.slot;
        let decision = match decisions_by_slot.get(slot).map(Vec::as_slice) {
            None | Some([]) => return Err(TimelineManifestError::MissingDecision(slot.clone())),
            Some([decision]) => *decision,
            Some(_) => return Err(TimelineManifestError::DuplicateDecision(slot.clone())),
        };
        match decision {
            TimelineSourceDecision::Omit {
                slot,
                collection,
                reason,
            } => {
                if expected_source.requirement == TimelineSlotRequirement::Required {
                    return Err(TimelineManifestError::RequiredSourceOmitted(slot.clone()));
                }
                if collection.trim().is_empty() {
                    return Err(TimelineManifestError::IncompleteSourceEvidence(
                        slot.clone(),
                    ));
                }
                omissions.push(TimelineManifestOmission {
                    slot: slot.clone(),
                    collection: collection.clone(),
                    reason: *reason,
                });
            }
            TimelineSourceDecision::Include {
                slot,
                collection,
                collection_version_id,
                exact,
            } => {
                if collection.trim().is_empty()
                    || collection_version_id.trim().is_empty()
                    || !complete_exact(exact)
                {
                    return Err(TimelineManifestError::IncompleteSourceEvidence(
                        slot.clone(),
                    ));
                }
                let source = match observed_by_slot.get(slot).map(Vec::as_slice) {
                    None | Some([]) => {
                        return Err(TimelineManifestError::MissingObservedSource(slot.clone()));
                    }
                    Some([source]) => *source,
                    Some(_) => {
                        return Err(TimelineManifestError::DuplicateObservedSource(slot.clone()));
                    }
                };
                if source.collection != *collection {
                    return Err(TimelineManifestError::SourceCollectionMismatch(
                        slot.clone(),
                    ));
                }
                if source.collection_version_id != *collection_version_id {
                    return Err(TimelineManifestError::SourceCollectionVersionMismatch(
                        slot.clone(),
                    ));
                }
                if source.exact != *exact {
                    return Err(TimelineManifestError::SourceVersionMismatch(slot.clone()));
                }
                sources.push(TimelineManifestSource {
                    slot: slot.clone(),
                    collection: collection.clone(),
                    collection_version_id: collection_version_id.clone(),
                    exact: exact.clone(),
                });
            }
        }
    }

    let root_source = sources
        .iter()
        .find(|source| source.slot == TimelineSourceSlot::root());
    if !matches!(
        root_source,
        Some(source) if source.collection == "AgentRequest" && source.exact == root
    ) {
        return Err(TimelineManifestError::RootDecisionMismatch);
    }
    Ok(RunTimelineSourceManifest {
        manifest_version: RUN_TIMELINE_MANIFEST_VERSION,
        status: if coverage_gaps.is_empty() && omissions.is_empty() {
            TimelineManifestStatus::VerifiedExact
        } else {
            TimelineManifestStatus::PartialExact
        },
        root,
        sources,
        omissions,
        coverage_gaps: coverage_gaps.to_vec(),
    })
}

/// Freeze a manifest and require closure over every exact edge recursively
/// discovered inside its source payloads.
pub fn freeze_timeline_manifest_with_declared_edges(
    selector: &TimelineRootSelector,
    candidates: &[TimelineRootCandidate],
    expected: &[TimelineExpectedSlot],
    observed: &[TimelineObservedSource],
    decisions: &[TimelineSourceDecision],
    coverage_gaps: &[TimelineCoverageGap],
    declared_edges: &[TimelineDeclaredExactEdge],
) -> Result<RunTimelineSourceManifest, TimelineManifestError> {
    let manifest = freeze_timeline_manifest(
        selector,
        candidates,
        expected,
        observed,
        decisions,
        coverage_gaps,
    )?;
    if let Some(edge) = declared_edges.iter().find(|edge| {
        !manifest.sources.iter().any(|source| {
            source.collection == edge.collection
                && source.collection_version_id == edge.collection_version_id
                && source.exact == edge.exact
        })
    }) {
        return Err(TimelineManifestError::MissingDeclaredExactEdge(
            edge.clone(),
        ));
    }
    Ok(manifest)
}

impl RunTimelineSourceManifest {
    /// Preserve every permitted exact source while replacing denied source
    /// slots with explicit omissions. The root request cannot be redacted: it
    /// defines the projection being requested and callers must reject the
    /// projection before reaching this method when root access is denied.
    pub fn with_denied_sources(
        &self,
        denied: &BTreeSet<(String, String)>,
    ) -> Result<Self, TimelineManifestError> {
        if denied.is_empty() {
            return Ok(self.clone());
        }

        let mut filtered = self.clone();
        filtered.sources.clear();
        let mut denied_source_count = 0_usize;
        for source in &self.sources {
            let key = (
                source.collection.clone(),
                source.exact.version.doc_id.clone(),
            );
            if !denied.contains(&key) {
                filtered.sources.push(source.clone());
                continue;
            }
            if source.slot == TimelineSourceSlot::root() {
                return Err(TimelineManifestError::RequiredSourceOmitted(
                    source.slot.clone(),
                ));
            }
            denied_source_count += 1;
            filtered.omissions.push(TimelineManifestOmission {
                slot: source.slot.clone(),
                collection: source.collection.clone(),
                reason: TimelineOmissionReason::Denied,
            });
        }
        if denied_source_count == 0 {
            return Err(TimelineManifestError::NonCanonicalManifest);
        }
        filtered
            .omissions
            .sort_by(|left, right| left.slot.cmp(&right.slot));
        filtered.status = TimelineManifestStatus::PartialExact;
        filtered.validate()?;
        Ok(filtered)
    }

    /// Validate a deserialized manifest by reconstructing the canonical freeze
    /// inputs and requiring byte-shape-equivalent output. This proves the
    /// structural contract only; cryptographic signer verification remains a
    /// property of the local loader that originally produced the manifest.
    pub fn validate(&self) -> Result<(), TimelineManifestError> {
        if self.manifest_version != RUN_TIMELINE_MANIFEST_VERSION {
            return Err(TimelineManifestError::UnsupportedManifestVersion(
                self.manifest_version,
            ));
        }

        let mut expected = Vec::with_capacity(self.sources.len() + self.omissions.len());
        let mut observed = Vec::with_capacity(self.sources.len());
        let mut decisions = Vec::with_capacity(self.sources.len() + self.omissions.len());
        for source in &self.sources {
            expected.push(TimelineExpectedSlot {
                slot: source.slot.clone(),
                requirement: TimelineSlotRequirement::Required,
            });
            observed.push(TimelineObservedSource {
                slot: source.slot.clone(),
                collection: source.collection.clone(),
                collection_version_id: source.collection_version_id.clone(),
                exact: source.exact.clone(),
            });
            decisions.push(TimelineSourceDecision::Include {
                slot: source.slot.clone(),
                collection: source.collection.clone(),
                collection_version_id: source.collection_version_id.clone(),
                exact: source.exact.clone(),
            });
        }
        for omission in &self.omissions {
            expected.push(TimelineExpectedSlot {
                slot: omission.slot.clone(),
                requirement: TimelineSlotRequirement::Optional,
            });
            decisions.push(TimelineSourceDecision::Omit {
                slot: omission.slot.clone(),
                collection: omission.collection.clone(),
                reason: omission.reason,
            });
        }
        expected.sort_by(|left, right| left.slot.cmp(&right.slot));
        observed.sort_by(|left, right| left.slot.cmp(&right.slot));
        decisions.sort_by(|left, right| left.slot().cmp(right.slot()));

        let canonical = freeze_timeline_manifest(
            &TimelineRootSelector::Exact(self.root.clone()),
            &[TimelineRootCandidate {
                request_id: String::new(),
                exact: self.root.clone(),
                current_head_count: 1,
            }],
            &expected,
            &observed,
            &decisions,
            &self.coverage_gaps,
        )?;
        if canonical != *self {
            return Err(TimelineManifestError::NonCanonicalManifest);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentVersionRef;

    fn exact(doc_id: &str, cid: &str) -> SignedDocumentVersionRef {
        SignedDocumentVersionRef {
            version: DocumentVersionRef {
                doc_id: doc_id.to_string(),
                composite_commit_cid: cid.to_string(),
            },
            signer_did: "did:key:agent".to_string(),
        }
    }

    #[test]
    fn logical_twins_fail_closed_but_exact_selection_is_stable() {
        let left = exact("request-left", "cid-left");
        let right = exact("request-right", "cid-right");
        let candidates = [
            TimelineRootCandidate {
                request_id: "logical".to_string(),
                exact: left.clone(),
                current_head_count: 1,
            },
            TimelineRootCandidate {
                request_id: "logical".to_string(),
                exact: right,
                current_head_count: 1,
            },
        ];
        assert!(matches!(
            resolve_timeline_root(
                &TimelineRootSelector::LogicalRequestId("logical".to_string()),
                &candidates
            ),
            Err(TimelineManifestError::AmbiguousRoot { .. })
        ));
        assert_eq!(
            resolve_timeline_root(&TimelineRootSelector::Exact(left.clone()), &candidates),
            Ok(left)
        );
    }

    #[test]
    fn exact_root_does_not_depend_on_later_current_head_count() {
        let root = exact("request-doc", "historical-cid");
        let candidates = [TimelineRootCandidate {
            request_id: "logical".to_string(),
            exact: root.clone(),
            current_head_count: 2,
        }];
        assert_eq!(
            resolve_timeline_root(&TimelineRootSelector::Exact(root.clone()), &candidates),
            Ok(root)
        );
        assert!(matches!(
            resolve_timeline_root(
                &TimelineRootSelector::LogicalRequestId("logical".to_string()),
                &candidates
            ),
            Err(TimelineManifestError::RootHasAmbiguousHeads { .. })
        ));
    }

    #[test]
    fn deserialized_manifest_must_match_the_canonical_contract() {
        let root = exact("request-doc", "request-cid");
        let manifest =
            root_only_timeline_manifest(root.clone(), "bafy-schema-agent-request").unwrap();
        manifest.validate().unwrap();

        let mut unsupported = manifest.clone();
        unsupported.manifest_version += 1;
        assert!(matches!(
            unsupported.validate(),
            Err(TimelineManifestError::UnsupportedManifestVersion(_))
        ));

        let mut wrong_collection = manifest.clone();
        wrong_collection.sources[0].collection = "AgentMessage".to_string();
        assert!(matches!(
            wrong_collection.validate(),
            Err(TimelineManifestError::RootDecisionMismatch)
        ));

        let mut incomplete = manifest;
        incomplete.sources[0].exact.signer_did.clear();
        assert!(matches!(
            incomplete.validate(),
            Err(TimelineManifestError::IncompleteSourceEvidence(_))
        ));

        let mut missing_schema_version =
            root_only_timeline_manifest(root, "bafy-schema-agent-request").unwrap();
        missing_schema_version.sources[0]
            .collection_version_id
            .clear();
        assert!(matches!(
            missing_schema_version.validate(),
            Err(TimelineManifestError::IncompleteSourceEvidence(_))
        ));
    }

    #[test]
    fn coverage_gaps_are_canonical_and_determine_status() {
        let root = exact("request-doc", "request-cid");
        let mut manifest = root_only_timeline_manifest(root, "bafy-schema-agent-request").unwrap();
        assert_eq!(manifest.manifest_version, 2);
        assert_eq!(manifest.status, TimelineManifestStatus::VerifiedExact);

        let gap = TimelineCoverageGap {
            kind: TimelineCoverageGapKind::OpenLogicalExtent,
            source_class: TimelineSourceClass::Message,
            collection: "AgentMessage".to_string(),
            scope_id: "request-doc".to_string(),
        };
        manifest.coverage_gaps.push(gap.clone());
        manifest.status = TimelineManifestStatus::PartialExact;
        manifest.validate().unwrap();

        let mut duplicate = manifest.clone();
        duplicate.coverage_gaps.push(gap);
        assert!(matches!(
            duplicate.validate(),
            Err(TimelineManifestError::DuplicateCoverageGap(_))
        ));

        let mut status_mismatch = manifest;
        status_mismatch.status = TimelineManifestStatus::VerifiedExact;
        assert!(matches!(
            status_mismatch.validate(),
            Err(TimelineManifestError::NonCanonicalManifest)
        ));

        let root = exact("request-doc", "request-cid");
        let mut incomplete =
            root_only_timeline_manifest(root, "bafy-schema-agent-request").unwrap();
        incomplete.coverage_gaps.push(TimelineCoverageGap {
            kind: TimelineCoverageGapKind::OpenSessionExtent,
            source_class: TimelineSourceClass::SessionProjection,
            collection: "AgentSession".to_string(),
            scope_id: " ".to_string(),
        });
        incomplete.status = TimelineManifestStatus::PartialExact;
        assert!(matches!(
            incomplete.validate(),
            Err(TimelineManifestError::IncompleteCoverageGap(_))
        ));
    }

    #[test]
    fn explicit_omission_is_partial_exact_without_coverage_gaps() {
        let root = exact("request-doc", "request-cid");
        let optional_slot = TimelineSourceSlot::new(TimelineSourceClass::RenderedRequest, 0);
        let manifest = freeze_timeline_manifest(
            &TimelineRootSelector::Exact(root.clone()),
            &[TimelineRootCandidate {
                request_id: "logical".to_string(),
                exact: root.clone(),
                current_head_count: 1,
            }],
            &[
                TimelineExpectedSlot {
                    slot: TimelineSourceSlot::root(),
                    requirement: TimelineSlotRequirement::Required,
                },
                TimelineExpectedSlot {
                    slot: optional_slot.clone(),
                    requirement: TimelineSlotRequirement::Optional,
                },
            ],
            &[TimelineObservedSource {
                slot: TimelineSourceSlot::root(),
                collection: "AgentRequest".to_string(),
                collection_version_id: "request-schema".to_string(),
                exact: root.clone(),
            }],
            &[
                TimelineSourceDecision::Include {
                    slot: TimelineSourceSlot::root(),
                    collection: "AgentRequest".to_string(),
                    collection_version_id: "request-schema".to_string(),
                    exact: root,
                },
                TimelineSourceDecision::Omit {
                    slot: optional_slot,
                    collection: "RenderedRequest".to_string(),
                    reason: TimelineOmissionReason::NotProduced,
                },
            ],
            &[],
        )
        .unwrap();

        assert_eq!(manifest.status, TimelineManifestStatus::PartialExact);
        manifest.validate().unwrap();
    }

    #[test]
    fn denied_source_becomes_an_explicit_partial_exact_omission() {
        let root = exact("request-doc", "request-cid");
        let message = exact("message-doc", "message-cid");
        let message_slot = TimelineSourceSlot::new(TimelineSourceClass::Message, 0);
        let manifest = freeze_timeline_manifest(
            &TimelineRootSelector::Exact(root.clone()),
            &[TimelineRootCandidate {
                request_id: "logical".to_string(),
                exact: root.clone(),
                current_head_count: 1,
            }],
            &[
                TimelineExpectedSlot {
                    slot: TimelineSourceSlot::root(),
                    requirement: TimelineSlotRequirement::Required,
                },
                TimelineExpectedSlot {
                    slot: message_slot.clone(),
                    requirement: TimelineSlotRequirement::Required,
                },
            ],
            &[
                TimelineObservedSource {
                    slot: TimelineSourceSlot::root(),
                    collection: "AgentRequest".to_string(),
                    collection_version_id: "request-schema".to_string(),
                    exact: root.clone(),
                },
                TimelineObservedSource {
                    slot: message_slot.clone(),
                    collection: "AgentMessage".to_string(),
                    collection_version_id: "message-schema".to_string(),
                    exact: message.clone(),
                },
            ],
            &[
                TimelineSourceDecision::Include {
                    slot: TimelineSourceSlot::root(),
                    collection: "AgentRequest".to_string(),
                    collection_version_id: "request-schema".to_string(),
                    exact: root,
                },
                TimelineSourceDecision::Include {
                    slot: message_slot.clone(),
                    collection: "AgentMessage".to_string(),
                    collection_version_id: "message-schema".to_string(),
                    exact: message,
                },
            ],
            &[],
        )
        .unwrap();

        let filtered = manifest
            .with_denied_sources(&BTreeSet::from([(
                "AgentMessage".to_string(),
                "message-doc".to_string(),
            )]))
            .unwrap();

        assert_eq!(filtered.status, TimelineManifestStatus::PartialExact);
        assert_eq!(filtered.sources.len(), 1);
        assert_eq!(filtered.sources[0].slot, TimelineSourceSlot::root());
        assert_eq!(
            filtered.omissions,
            vec![TimelineManifestOmission {
                slot: message_slot,
                collection: "AgentMessage".to_string(),
                reason: TimelineOmissionReason::Denied,
            }]
        );
        filtered.validate().unwrap();
    }

    #[test]
    fn deserialization_fails_closed_on_missing_unknown_or_forged_coverage() {
        let root = exact("request-doc", "request-cid");
        let verified = root_only_timeline_manifest(root, "bafy-schema-agent-request").unwrap();
        let mut missing = serde_json::to_value(&verified).unwrap();
        missing.as_object_mut().unwrap().remove("coverage_gaps");
        assert!(serde_json::from_value::<RunTimelineSourceManifest>(missing).is_err());

        let mut null = serde_json::to_value(&verified).unwrap();
        null["coverage_gaps"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<RunTimelineSourceManifest>(null).is_err());

        let mut partial = verified;
        partial.status = TimelineManifestStatus::PartialExact;
        partial.coverage_gaps = vec![TimelineCoverageGap {
            kind: TimelineCoverageGapKind::OpenLogicalExtent,
            source_class: TimelineSourceClass::Message,
            collection: "AgentMessage".to_string(),
            scope_id: "request-doc".to_string(),
        }];
        partial.validate().unwrap();

        let mut forged = serde_json::to_value(&partial).unwrap();
        forged["status"] = serde_json::Value::String("verified_exact".to_string());
        assert!(serde_json::from_value::<RunTimelineSourceManifest>(forged).is_err());

        let mut unknown = serde_json::to_value(&partial).unwrap();
        unknown["coverage_gaps"][0]["kind"] =
            serde_json::Value::String("future_gap_kind".to_string());
        assert!(serde_json::from_value::<RunTimelineSourceManifest>(unknown).is_err());

        for pointer in [
            "",
            "/root",
            "/root/version",
            "/sources/0",
            "/sources/0/slot",
            "/sources/0/exact",
            "/sources/0/exact/version",
            "/coverage_gaps/0",
        ] {
            let mut value = serde_json::to_value(&partial).unwrap();
            let object = if pointer.is_empty() {
                value.as_object_mut().unwrap()
            } else {
                value.pointer_mut(pointer).unwrap().as_object_mut().unwrap()
            };
            object.insert("unexpected".to_string(), serde_json::Value::Bool(true));
            let error = serde_json::from_value::<RunTimelineSourceManifest>(value)
                .expect_err("unknown manifest fields must fail closed");
            assert!(
                error.to_string().contains("unknown field `unexpected`"),
                "pointer {pointer}: {error}"
            );
        }

        let mut omission = serde_json::to_value(&partial).unwrap();
        omission["omissions"] = serde_json::json!([{
            "slot": { "source_class": "rendered_request", "ordinal": 0 },
            "collection": "RenderedRequest",
            "reason": "not_produced"
        }]);
        serde_json::from_value::<RunTimelineSourceManifest>(omission.clone()).unwrap();
        omission["omissions"][0]["unexpected"] = serde_json::Value::Bool(true);
        let error = serde_json::from_value::<RunTimelineSourceManifest>(omission)
            .expect_err("unknown omission fields must fail closed");
        assert!(error.to_string().contains("unknown field `unexpected`"));
    }
}
