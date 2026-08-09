import Proofs.Conformance.ContractCases.Types
import Proofs.RunTimelineManifest

namespace Conformance.ContractCases

open RunTimelineManifest

structure RunTimelineManifestCase where
  name : String
  disposition : String
  selector : String
  visibleLogicalRoots : Nat
  rootDocId : Option Nat
  rootCid : Option Nat
  expectedSlots : Nat
  includedSlots : Nat
  omittedSlots : Nat
  orderedSourceClasses : List String
  orderedCollections : List Nat
  orderedCollectionVersionIds : List Nat
  orderedDocIds : List Nat
  orderedCids : List Nat
  exactMembership : Bool
  completeCoverage : Bool
  canonicalOrder : Bool
  manifestVersion : Option Nat
  manifestStatus : Option String
  coverageGapCount : Nat
  orderedCoverageGapKinds : List String
  canonicalGaps : Bool
  deriving Repr

private def signed (docId cid : Nat) (valid : Bool := true) : ToolFact.SignedRef :=
  { version := { docId, compositeCommitCid := cid }
  , signerDid := 7
  , signatureValid := valid }

private def requestRef := signed 101 1001
private def outcomeRef := signed 201 2001
private def callRef := signed 301 3001
private def renderRef := signed 401 4001

private def requestCollection := 10
private def requestCollectionVersion := 100
private def outcomeCollection := 20
private def outcomeCollectionVersion := 200
private def callCollection := 30
private def callCollectionVersion := 300
private def renderCollection := 40
private def renderCollectionVersion := 400

private def slot (sourceClass : SourceClass) (ordinal : Nat := 0) : SourceSlot :=
  ⟨sourceClass, ordinal⟩

private def requestSlot := slot .request
private def outcomeSlot := slot .responseOutcome
private def callSlot := slot .inferenceCall
private def renderSlot := slot .renderedRequest

private def seenSource
    (sourceSlot : SourceSlot) (collection collectionVersionId : Nat)
    (exact : ToolFact.SignedRef) : ObservedSource :=
  ⟨sourceSlot, collection, collectionVersionId, exact⟩

private def includeSource
    (sourceSlot : SourceSlot) (collection collectionVersionId : Nat)
    (exact : ToolFact.SignedRef) : SlotDecision :=
  .include sourceSlot collection collectionVersionId exact

private def expected : List ExpectedSlot :=
  [ ⟨requestSlot, .required⟩
  , ⟨outcomeSlot, .required⟩
  , ⟨callSlot, .required⟩
  , ⟨renderSlot, .optional⟩ ]

private def observed : List ObservedSource :=
  [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
  , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
  , seenSource callSlot callCollection callCollectionVersion callRef
  , seenSource renderSlot renderCollection renderCollectionVersion renderRef ]

private def decisions : List SlotDecision :=
  [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
  , includeSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
  , includeSource callSlot callCollection callCollectionVersion callRef
  , includeSource renderSlot renderCollection renderCollectionVersion renderRef ]

private def uniqueRoot : List RootCandidate := [⟨1, requestRef, 1⟩]

private def sourceClassContract : SourceClass → String
  | .request => "request"
  | .sessionProjection => "session_projection"
  | .conversationProjection => "conversation_projection"
  | .message => "message"
  | .toolCall => "tool_call"
  | .toolResult => "tool_result"
  | .toolApproval => "tool_approval"
  | .responseLive => "response_live"
  | .responseOutcome => "response_outcome"
  | .inferenceCall => "inference_call"
  | .renderedRequest => "rendered_request"
  | .resolvedConfig => "resolved_config"
  | .compaction => "compaction"

private def selectorString : RootSelector → String
  | .exact _ => "exact"
  | .logical _ => "logical"

private def coverageGapKindContract : CoverageGapKind → String
  | .openLogicalExtent => "open_logical_extent"
  | .openSessionExtent => "open_session_extent"
  | .nonAtomicObservation => "non_atomic_observation"
  | .remoteSignatureUnverified => "remote_signature_unverified"

private def manifestStatusContract : ManifestStatus → String
  | .verifiedExact => "verified_exact"
  | .partialExact => "partial_exact"

private def gap (kind : CoverageGapKind) (sourceClass : SourceClass)
    (collection scopeId : Nat) : CoverageGap :=
  ⟨kind, sourceClass, collection, scopeId⟩

private def includedRefs (items : List SlotDecision) : List ToolFact.SignedRef :=
  items.filterMap fun
    | .include _ _ _ exact => some exact
    | .omit _ _ _ => none

private def includedCollections (items : List SlotDecision) : List Nat :=
  items.filterMap fun
    | .include _ collection _ _ => some collection
    | .omit _ _ _ => none

private def includedCollectionVersions (items : List SlotDecision) : List Nat :=
  items.filterMap fun
    | .include _ _ collectionVersionId _ => some collectionVersionId
    | .omit _ _ _ => none

private def sourceClasses (items : List SlotDecision) : List String :=
  items.map fun item => sourceClassContract item.slot.sourceClass

private def exactMembership (manifest : Manifest) (seen : List ObservedSource) : Bool :=
  manifest.items.all fun
    | .omit _ _ _ => true
    | .include sourceSlot collection collectionVersionId exact =>
        observedSource? sourceSlot seen ==
          some ⟨sourceSlot, collection, collectionVersionId, exact⟩

private def caseWithGapsAndEdges
    (name : String)
    (selector : RootSelector)
    (candidates : List RootCandidate)
    (expectedSlots : List ExpectedSlot)
    (seen : List ObservedSource)
    (slotDecisions : List SlotDecision)
    (coverageGaps : List CoverageGap)
    (declaredEdges : List DeclaredExactEdge) : RunTimelineManifestCase :=
  let result := freezeWithDeclaredEdges? selector candidates expectedSlots seen slotDecisions
    coverageGaps declaredEdges
  let manifestItems := result.map (·.items) |>.getD []
  let refs := includedRefs manifestItems
  { name
  , disposition := if result.isSome then "accepted" else "rejected"
  , selector := selectorString selector
  , visibleLogicalRoots := candidates.length
  , rootDocId := result.map (·.root.version.docId)
  , rootCid := result.map (·.root.version.compositeCommitCid)
  , expectedSlots := expectedSlots.length
  , includedSlots := refs.length
  , omittedSlots := manifestItems.length - refs.length
  , orderedSourceClasses := sourceClasses manifestItems
  , orderedCollections := includedCollections manifestItems
  , orderedCollectionVersionIds := includedCollectionVersions manifestItems
  , orderedDocIds := refs.map (·.version.docId)
  , orderedCids := refs.map (·.version.compositeCommitCid)
  , exactMembership := result.any fun manifest => exactMembership manifest seen
  , completeCoverage := result.any fun manifest =>
      manifest.items.map SlotDecision.slot == expectedSlots.map (·.slot)
  , canonicalOrder := slotsStrictlyOrdered (expectedSlots.map (·.slot))
  , manifestVersion := result.map (·.version)
  , manifestStatus := result.map (manifestStatusContract ·.status)
  , coverageGapCount := result.map (·.coverageGaps.length) |>.getD 0
  , orderedCoverageGapKinds := result.map
      (fun manifest => manifest.coverageGaps.map (coverageGapKindContract ·.kind)) |>.getD []
  , canonicalGaps := gapsStrictlyOrdered coverageGaps }

private def caseWithGaps
    (name : String)
    (selector : RootSelector)
    (candidates : List RootCandidate)
    (expectedSlots : List ExpectedSlot)
    (seen : List ObservedSource)
    (slotDecisions : List SlotDecision)
    (coverageGaps : List CoverageGap) : RunTimelineManifestCase :=
  caseWithGapsAndEdges name selector candidates expectedSlots seen slotDecisions coverageGaps []

private def caseOf
    (name : String)
    (selector : RootSelector)
    (candidates : List RootCandidate)
    (expectedSlots : List ExpectedSlot)
    (seen : List ObservedSource)
    (slotDecisions : List SlotDecision) : RunTimelineManifestCase :=
  caseWithGaps name selector candidates expectedSlots seen slotDecisions []

private def wrongRequestCid := signed 101 1999
private def twinRequest := signed 102 1002
private def changedRenderCid := signed 401 4999
private def changedRenderDoc := signed 499 4001
private def changedRenderSigner : ToolFact.SignedRef :=
  { renderRef with signerDid := 8 }
private def unsignedRender := signed 401 4001 false
private def changedRenderCollection := 49
private def changedRenderCollectionVersion := 499
private def responseLiveSlot := slot .responseLive
private def compactionSlot := slot .compaction
private def observedWithoutRender : List ObservedSource :=
  [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
  , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
  , seenSource callSlot callCollection callCollectionVersion callRef ]

private def canonicalCoverageGaps : List CoverageGap :=
  [ gap .openLogicalExtent .message 60 1
  , gap .openSessionExtent .sessionProjection 70 2
  , gap .nonAtomicObservation .responseOutcome 20 1
  , gap .remoteSignatureUnverified .renderedRequest 40 1 ]

private def messageRef := signed 601 6001
private def messageCollection := 60
private def messageCollectionVersion := 600
private def messageSlot := slot .message
private def expectedWithMessage : List ExpectedSlot :=
  [ ⟨requestSlot, .required⟩, ⟨messageSlot, .required⟩, ⟨outcomeSlot, .required⟩
  , ⟨callSlot, .required⟩, ⟨renderSlot, .optional⟩ ]
private def observedWithMessage : List ObservedSource :=
  [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
  , seenSource messageSlot messageCollection messageCollectionVersion messageRef
  , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
  , seenSource callSlot callCollection callCollectionVersion callRef
  , seenSource renderSlot renderCollection renderCollectionVersion renderRef ]
private def decisionsWithMessage : List SlotDecision :=
  [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
  , includeSource messageSlot messageCollection messageCollectionVersion messageRef
  , includeSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
  , includeSource callSlot callCollection callCollectionVersion callRef
  , includeSource renderSlot renderCollection renderCollectionVersion renderRef ]
private def messageEdge : DeclaredExactEdge :=
  ⟨messageCollection, messageCollectionVersion, messageRef⟩

def runTimelineManifestCases : List RunTimelineManifestCase :=
  [ caseOf "exact_root_selected" (.exact requestRef) uniqueRoot expected observed decisions
  , caseOf "unique_logical_root_selected" (.logical 1) uniqueRoot expected observed decisions
  , caseOf "missing_logical_root_rejected" (.logical 1) [] expected observed decisions
  , caseOf "ambiguous_logical_root_rejected" (.logical 1)
      [⟨1, requestRef, 1⟩, ⟨1, twinRequest, 1⟩] expected observed decisions
  , caseOf "exact_root_wrong_cid_rejected" (.exact wrongRequestCid) uniqueRoot
      expected observed decisions
  , caseOf "exact_root_multiple_heads_selected" (.exact requestRef)
      [⟨1, requestRef, 2⟩] expected observed decisions
  , caseOf "unsigned_root_rejected" (.logical 1)
      [⟨1, signed 101 1001 false, 1⟩] expected observed decisions
  , caseOf "exact_sources_frozen" (.exact requestRef) uniqueRoot expected observed decisions
  , caseWithGapsAndEdges "nested_provenance_edge_frozen" (.exact requestRef) uniqueRoot
      expectedWithMessage observedWithMessage decisionsWithMessage [] [messageEdge]
  , caseWithGapsAndEdges "missing_nested_provenance_edge_rejected" (.exact requestRef)
      uniqueRoot expected observed decisions [] [messageEdge]
  , caseWithGapsAndEdges "nested_provenance_schema_rebind_rejected" (.exact requestRef)
      uniqueRoot expectedWithMessage observedWithMessage decisionsWithMessage []
      [⟨messageCollection, changedRenderCollectionVersion, messageRef⟩]
  , caseWithGapsAndEdges "nested_provenance_signer_rebind_rejected" (.exact requestRef)
      uniqueRoot expectedWithMessage observedWithMessage decisionsWithMessage []
      [⟨messageCollection, messageCollectionVersion, { messageRef with signerDid := 8 }⟩]
  , caseOf "source_cid_rebind_rejected" (.exact requestRef) uniqueRoot expected
      [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
      , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , seenSource callSlot callCollection callCollectionVersion callRef
      , seenSource renderSlot renderCollection renderCollectionVersion changedRenderCid ] decisions
  , caseOf "source_doc_rebind_rejected" (.exact requestRef) uniqueRoot expected
      [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
      , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , seenSource callSlot callCollection callCollectionVersion callRef
      , seenSource renderSlot renderCollection renderCollectionVersion changedRenderDoc ] decisions
  , caseOf "source_signer_rebind_rejected" (.exact requestRef) uniqueRoot expected
      [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
      , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , seenSource callSlot callCollection callCollectionVersion callRef
      , seenSource renderSlot renderCollection renderCollectionVersion changedRenderSigner ] decisions
  , caseOf "source_collection_rebind_rejected" (.exact requestRef) uniqueRoot expected
      [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
      , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , seenSource callSlot callCollection callCollectionVersion callRef
      , seenSource renderSlot changedRenderCollection renderCollectionVersion renderRef ] decisions
  , caseOf "source_schema_version_rebind_rejected" (.exact requestRef) uniqueRoot expected
      [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
      , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , seenSource callSlot callCollection callCollectionVersion callRef
      , seenSource renderSlot renderCollection changedRenderCollectionVersion renderRef ] decisions
  , caseOf "unsigned_source_rejected" (.exact requestRef) uniqueRoot expected
      [ seenSource requestSlot requestCollection requestCollectionVersion requestRef
      , seenSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , seenSource callSlot callCollection callCollectionVersion callRef
      , seenSource renderSlot renderCollection renderCollectionVersion unsignedRender ]
      [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
      , includeSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , includeSource callSlot callCollection callCollectionVersion callRef
      , includeSource renderSlot renderCollection renderCollectionVersion unsignedRender ]
  , caseOf "duplicate_observed_slot_rejected" (.exact requestRef) uniqueRoot expected
      (observed ++ [seenSource renderSlot renderCollection renderCollectionVersion renderRef])
      decisions
  , caseOf "undeclared_source_decision_rejected" (.exact requestRef) uniqueRoot expected
      observed (decisions ++ [.omit (slot .compaction) 50 .notApplicable])
  , caseOf "undeclared_observed_source_rejected" (.exact requestRef) uniqueRoot expected
      (observed ++ [seenSource compactionSlot 50 500 (signed 501 5001)]) decisions
  , caseOf "reversed_decision_input_emits_canonical_order" (.exact requestRef) uniqueRoot
      expected observed decisions.reverse
  , caseOf "duplicate_expected_slot_rejected" (.exact requestRef) uniqueRoot
      (expected ++ [⟨renderSlot, .optional⟩]) observed decisions
  , caseOf "noncanonical_expected_order_rejected" (.exact requestRef) uniqueRoot
      expected.reverse observed decisions
  , caseOf "optional_unsent_render_explicitly_omitted" (.exact requestRef) uniqueRoot
      expected observedWithoutRender
      [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
      , includeSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , includeSource callSlot callCollection callCollectionVersion callRef
      , .omit renderSlot renderCollection .notProduced ]
  , caseOf "optional_redacted_render_explicitly_omitted" (.exact requestRef) uniqueRoot
      expected observed
      [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
      , includeSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , includeSource callSlot callCollection callCollectionVersion callRef
      , .omit renderSlot renderCollection .redacted ]
  , caseOf "optional_live_projection_explicitly_omitted" (.exact requestRef) uniqueRoot
      [ ⟨requestSlot, .required⟩, ⟨responseLiveSlot, .optional⟩
      , ⟨outcomeSlot, .required⟩, ⟨callSlot, .required⟩, ⟨renderSlot, .optional⟩ ]
      observed
      [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
      , .omit responseLiveSlot 15 .projectionExcluded
      , includeSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , includeSource callSlot callCollection callCollectionVersion callRef
      , includeSource renderSlot renderCollection renderCollectionVersion renderRef ]
  , caseOf "required_source_omission_rejected" (.exact requestRef) uniqueRoot expected
      observed
      [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
      , .omit outcomeSlot outcomeCollection .legacyUnavailable
      , includeSource callSlot callCollection callCollectionVersion callRef
      , includeSource renderSlot renderCollection renderCollectionVersion renderRef ]
  , caseOf "missing_decision_rejected" (.exact requestRef) uniqueRoot expected observed
      [ includeSource requestSlot requestCollection requestCollectionVersion requestRef
      , includeSource outcomeSlot outcomeCollection outcomeCollectionVersion outcomeRef
      , includeSource callSlot callCollection callCollectionVersion callRef ]
  , caseWithGaps "canonical_coverage_gaps_are_partial_exact" (.exact requestRef)
      uniqueRoot expected observed decisions canonicalCoverageGaps
  , caseWithGaps "duplicate_coverage_gap_rejected" (.exact requestRef)
      uniqueRoot expected observed decisions
      [gap .openLogicalExtent .message 60 1, gap .openLogicalExtent .message 60 1]
  , caseWithGaps "noncanonical_coverage_gap_order_rejected" (.exact requestRef)
      uniqueRoot expected observed decisions canonicalCoverageGaps.reverse
  , caseWithGaps "remote_signature_gap_is_partial_exact" (.exact requestRef)
      uniqueRoot expected observed decisions
      [gap .remoteSignatureUnverified .renderedRequest 40 1] ]

theorem runTimelineManifestCases_pinned :
    runTimelineManifestCases.map (fun row => (row.name, row.disposition)) =
      [ ("exact_root_selected", "accepted")
      , ("unique_logical_root_selected", "accepted")
      , ("missing_logical_root_rejected", "rejected")
      , ("ambiguous_logical_root_rejected", "rejected")
      , ("exact_root_wrong_cid_rejected", "rejected")
      , ("exact_root_multiple_heads_selected", "accepted")
      , ("unsigned_root_rejected", "rejected")
      , ("exact_sources_frozen", "accepted")
      , ("nested_provenance_edge_frozen", "accepted")
      , ("missing_nested_provenance_edge_rejected", "rejected")
      , ("nested_provenance_schema_rebind_rejected", "rejected")
      , ("nested_provenance_signer_rebind_rejected", "rejected")
      , ("source_cid_rebind_rejected", "rejected")
      , ("source_doc_rebind_rejected", "rejected")
      , ("source_signer_rebind_rejected", "rejected")
      , ("source_collection_rebind_rejected", "rejected")
      , ("source_schema_version_rebind_rejected", "rejected")
      , ("unsigned_source_rejected", "rejected")
      , ("duplicate_observed_slot_rejected", "rejected")
      , ("undeclared_source_decision_rejected", "rejected")
      , ("undeclared_observed_source_rejected", "rejected")
      , ("reversed_decision_input_emits_canonical_order", "accepted")
      , ("duplicate_expected_slot_rejected", "rejected")
      , ("noncanonical_expected_order_rejected", "rejected")
      , ("optional_unsent_render_explicitly_omitted", "accepted")
      , ("optional_redacted_render_explicitly_omitted", "accepted")
      , ("optional_live_projection_explicitly_omitted", "accepted")
      , ("required_source_omission_rejected", "rejected")
      , ("missing_decision_rejected", "rejected")
      , ("canonical_coverage_gaps_are_partial_exact", "accepted")
      , ("duplicate_coverage_gap_rejected", "rejected")
      , ("noncanonical_coverage_gap_order_rejected", "rejected")
      , ("remote_signature_gap_is_partial_exact", "accepted") ] := by
  native_decide

theorem explicit_optional_omissions_are_partial_exact :
    List.map (fun row => (row.name, row.manifestStatus))
      (runTimelineManifestCases.filter (fun row =>
        [ "optional_unsent_render_explicitly_omitted"
        , "optional_redacted_render_explicitly_omitted"
        , "optional_live_projection_explicitly_omitted" ].contains row.name)) =
      [ ("optional_unsent_render_explicitly_omitted", some "partial_exact")
      , ("optional_redacted_render_explicitly_omitted", some "partial_exact")
      , ("optional_live_projection_explicitly_omitted", some "partial_exact") ] := by
  native_decide

theorem complete_included_source_set_is_verified_exact :
    (runTimelineManifestCases.find? (·.name == "exact_sources_frozen")
      |>.bind (·.manifestStatus)) = some "verified_exact" := by
  native_decide

end Conformance.ContractCases
