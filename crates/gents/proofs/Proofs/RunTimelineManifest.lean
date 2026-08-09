import Proofs.ToolFact

/-!
# Frozen run-timeline source manifests

A timeline or adapter projection is historical evidence only when it selects
one exact signed request root and records one decision for every source slot.
Logical identifiers discover candidates; they never choose among twins.

The query layer supplies its visible candidate/source sets. This model makes
the remaining boundary executable: exact selection, authoritative membership
in that observed set, explicit coverage gaps and omissions, and canonical
manifest order.
-/

namespace RunTimelineManifest

abbrev LogicalRequestId := Nat

inductive SourceClass where
  | request
  | sessionProjection
  | conversationProjection
  | message
  | toolCall
  | toolResult
  | toolApproval
  | responseLive
  | responseOutcome
  | inferenceCall
  | renderedRequest
  | resolvedConfig
  | compaction
  deriving BEq, DecidableEq, Repr

def SourceClass.rank : SourceClass → Nat
  | .request => 0
  | .sessionProjection => 1
  | .conversationProjection => 2
  | .message => 3
  | .toolCall => 4
  | .toolResult => 5
  | .toolApproval => 6
  | .responseLive => 7
  | .responseOutcome => 8
  | .inferenceCall => 9
  | .renderedRequest => 10
  | .resolvedConfig => 11
  | .compaction => 12

structure SourceSlot where
  sourceClass : SourceClass
  ordinal : Nat
  deriving BEq, DecidableEq, Repr

@[simp] theorem SourceSlot.decide_self (slot : SourceSlot) : decide (slot = slot) = true := by
  simp

def rootSlot : SourceSlot := ⟨.request, 0⟩

def SourceSlot.lt (left right : SourceSlot) : Bool :=
  left.sourceClass.rank < right.sourceClass.rank
    || (decide (left.sourceClass = right.sourceClass) && left.ordinal < right.ordinal)

def slotsStrictlyOrdered : List SourceSlot → Bool
  | [] | [_] => true
  | left :: right :: rest => left.lt right && slotsStrictlyOrdered (right :: rest)

structure RootCandidate where
  logicalId : LogicalRequestId
  exact : ToolFact.SignedRef
  currentHeadCount : Nat
  deriving DecidableEq, Repr

inductive RootSelector where
  | exact (source : ToolFact.SignedRef)
  | logical (requestId : LogicalRequestId)
  deriving DecidableEq, Repr

def RootCandidate.admissible (candidate : RootCandidate) : Bool :=
  candidate.currentHeadCount == 1 && candidate.exact.authoritative

def RootCandidate.admissibleFor
    (candidate : RootCandidate) (selector : RootSelector) : Bool :=
  match selector with
  | .exact _ => candidate.exact.authoritative
  | .logical _ => candidate.admissible

def rootMatches (selector : RootSelector) (candidate : RootCandidate) : Bool :=
  match selector with
  | .exact source => decide (candidate.exact = source)
  | .logical requestId => candidate.logicalId == requestId

/--
Resolve from the complete candidate set. Logical selection accepts exactly
one physical row with one current head. Exact selection requires one exact
authoritative snapshot but is independent of later current-head ambiguity.
-/
def selectRoot? (selector : RootSelector) (candidates : List RootCandidate) :
    Option ToolFact.SignedRef :=
  match candidates.filter (rootMatches selector) with
  | [candidate] =>
      if candidate.admissibleFor selector then some candidate.exact else none
  | _ => none

inductive OmissionReason where
  | notProduced
  | notApplicable
  | projectionExcluded
  | redacted
  | legacyUnavailable
  | denied
  | erased
  | unsupportedManifest
  | heuristicLogicalJoin
  | remoteSignatureUnverified
  deriving BEq, DecidableEq, Repr

inductive SlotRequirement where
  | required
  | optional
  deriving BEq, DecidableEq, Repr

structure ExpectedSlot where
  slot : SourceSlot
  requirement : SlotRequirement
  deriving DecidableEq, Repr

structure ObservedSource where
  slot : SourceSlot
  collection : Nat
  collectionVersionId : Nat
  exact : ToolFact.SignedRef
  deriving DecidableEq, Repr

inductive SlotDecision where
  | include (slot : SourceSlot) (collection collectionVersionId : Nat)
      (exact : ToolFact.SignedRef)
  | omit (slot : SourceSlot) (collection : Nat) (reason : OmissionReason)
  deriving DecidableEq, Repr

def SlotDecision.slot : SlotDecision → SourceSlot
  | .include slot _ _ _ | .omit slot _ _ => slot

def decisionFor? (slot : SourceSlot) (decisions : List SlotDecision) :
    Option SlotDecision :=
  match decisions.filter (fun decision => decide (decision.slot = slot)) with
  | [decision] => some decision
  | _ => none

@[simp] theorem decisionFor_single_include
    (slot : SourceSlot) (collection collectionVersionId : Nat)
    (exact : ToolFact.SignedRef) :
    decisionFor? slot [.include slot collection collectionVersionId exact] =
      some (.include slot collection collectionVersionId exact) := by
  simp [decisionFor?, SlotDecision.slot, SourceSlot.decide_self]

@[simp] theorem decisionFor_single_omit
    (slot : SourceSlot) (collection : Nat) (reason : OmissionReason) :
    decisionFor? slot [.omit slot collection reason] = some (.omit slot collection reason) := by
  simp [decisionFor?, SlotDecision.slot, SourceSlot.decide_self]

@[simp] theorem decisionFor_duplicate_include
    (slot : SourceSlot) (collection collectionVersionId : Nat)
    (exact : ToolFact.SignedRef) :
    decisionFor? slot
      [.include slot collection collectionVersionId exact,
       .include slot collection collectionVersionId exact] = none := by
  simp [decisionFor?, SlotDecision.slot, SourceSlot.decide_self]

def observedSource? (slot : SourceSlot) (observed : List ObservedSource) :
    Option ObservedSource :=
  match observed.filter (fun source => decide (source.slot = slot)) with
  | [source] => some source
  | _ => none

@[simp] theorem observedSource_single
    (slot : SourceSlot) (collection collectionVersionId : Nat)
    (exact : ToolFact.SignedRef) :
    observedSource? slot [⟨slot, collection, collectionVersionId, exact⟩] =
      some ⟨slot, collection, collectionVersionId, exact⟩ := by
  simp [observedSource?, SourceSlot.decide_self]

def expectedContains (expected : List ExpectedSlot) (slot : SourceSlot) : Bool :=
  expected.any (fun item => decide (item.slot = slot))

def decisionsDeclared (expected : List ExpectedSlot) (decisions : List SlotDecision) : Bool :=
  decisions.all (fun decision => expectedContains expected decision.slot)

/-- Every row returned in the exact observed set belongs to the declared source
policy. Without this check a newly visible collection/edge could be silently
dropped merely because no decision was requested for it. -/
def observedDeclared (expected : List ExpectedSlot) (observed : List ObservedSource) : Bool :=
  observed.all (fun source => expectedContains expected source.slot)

def freezeSlots?
    (expected : List ExpectedSlot)
    (observed : List ObservedSource)
    (decisions : List SlotDecision) : Option (List SlotDecision) :=
  match expected with
  | [] => some []
  | item :: rest => do
      let decision ← decisionFor? item.slot decisions
      match decision with
      | .include slot collection collectionVersionId exact =>
          if exact.authoritative then
            match observedSource? slot observed with
            | some current =>
                if current.collection = collection
                    && current.collectionVersionId = collectionVersionId
                    && current.exact = exact then
                  return decision :: (← freezeSlots? rest observed decisions)
                else none
            | none => none
          else none
      | .omit _ _ _ =>
          if item.requirement = .optional then
            return decision :: (← freezeSlots? rest observed decisions)
          else none

/-- A reason the query layer cannot claim that its exact observed membership
is also a closed-world account of every source that belongs to the run. -/
inductive CoverageGapKind where
  | openLogicalExtent
  | openSessionExtent
  | nonAtomicObservation
  | remoteSignatureUnverified
  deriving BEq, DecidableEq, Repr

def CoverageGapKind.rank : CoverageGapKind → Nat
  | .openLogicalExtent => 0
  | .openSessionExtent => 1
  | .nonAtomicObservation => 2
  | .remoteSignatureUnverified => 3

/-- Gap identity is semantic and stable: the kind, affected source class,
collection contract, and logical/session scope. -/
structure CoverageGap where
  kind : CoverageGapKind
  sourceClass : SourceClass
  collection : Nat
  scopeId : Nat
  deriving BEq, DecidableEq, Repr

def CoverageGap.lt (left right : CoverageGap) : Bool :=
  left.kind.rank < right.kind.rank
    || (left.kind.rank == right.kind.rank
      && (left.sourceClass.rank < right.sourceClass.rank
        || (left.sourceClass.rank == right.sourceClass.rank
          && (left.collection < right.collection
            || (left.collection == right.collection && left.scopeId < right.scopeId)))))

def gapsStrictlyOrdered : List CoverageGap → Bool
  | [] | [_] => true
  | left :: right :: rest => left.lt right && gapsStrictlyOrdered (right :: rest)

inductive ManifestStatus where
  | verifiedExact
  | partialExact
  deriving BEq, DecidableEq, Repr

def itemsHaveNoOmissions : List SlotDecision → Bool
  | [] => true
  | .include _ _ _ _ :: rest => itemsHaveNoOmissions rest
  | .omit _ _ _ :: _ => false

def statusFor (coverageGaps : List CoverageGap) (items : List SlotDecision) : ManifestStatus :=
  match coverageGaps with
  | _ :: _ => .partialExact
  | [] => if itemsHaveNoOmissions items then .verifiedExact else .partialExact

structure Manifest where
  version : Nat
  root : ToolFact.SignedRef
  items : List SlotDecision
  status : ManifestStatus
  coverageGaps : List CoverageGap
  deriving DecidableEq, Repr

def itemsHaveExactObservedMembership
    (observed : List ObservedSource) (items : List SlotDecision) : Bool :=
  items.all fun
    | .omit _ _ _ => true
    | .include slot collection collectionVersionId exact =>
        exact.authoritative && observedSource? slot observed ==
          some ⟨slot, collection, collectionVersionId, exact⟩

def rootDecisionMatches (root : ToolFact.SignedRef) (decisions : List SlotDecision) : Bool :=
  match decisionFor? rootSlot decisions with
  | some (.include _ _ _ exact) => decide (exact = root)
  | _ => false

/-- Freeze decisions in source-policy order, independent of query row order. -/
def freeze?
    (selector : RootSelector)
    (candidates : List RootCandidate)
    (expected : List ExpectedSlot)
    (observed : List ObservedSource)
    (decisions : List SlotDecision)
    (coverageGaps : List CoverageGap) : Option Manifest := do
  let root ← selectRoot? selector candidates
  if gapsStrictlyOrdered coverageGaps
      && slotsStrictlyOrdered (expected.map (·.slot))
      && decisionsDeclared expected decisions
      && observedDeclared expected observed
      && rootDecisionMatches root decisions then
    let items ← freezeSlots? expected observed decisions
    if decide (items.map SlotDecision.slot = expected.map (·.slot))
        && itemsHaveExactObservedMembership observed items then
      pure ⟨2, root, items, statusFor coverageGaps items, coverageGaps⟩
    else none
  else none

theorem ambiguous_logical_root_rejected
    (logicalId : LogicalRequestId) (left right : RootCandidate)
    (hLeft : left.logicalId = logicalId) (hRight : right.logicalId = logicalId) :
    selectRoot? (.logical logicalId) [left, right] = none := by
  simp [selectRoot?, rootMatches, hLeft, hRight]

theorem missing_logical_root_rejected (logicalId : LogicalRequestId) :
    selectRoot? (.logical logicalId) [] = none := by
  rfl

theorem unique_logical_root_selected
    (candidate : RootCandidate)
    (hAdmissible : candidate.admissible = true) :
    selectRoot? (.logical candidate.logicalId) [candidate] = some candidate.exact := by
  simp [selectRoot?, rootMatches, RootCandidate.admissibleFor, hAdmissible]

theorem exact_root_cid_rebind_rejected
    (wanted candidate : ToolFact.SignedRef)
    (hDifferent : candidate ≠ wanted)
    (logicalId : LogicalRequestId) :
    selectRoot? (.exact wanted) [⟨logicalId, candidate, 1⟩] = none := by
  simp [selectRoot?, rootMatches, hDifferent]

theorem multiple_current_heads_rejected
    (logicalId : LogicalRequestId) (exact : ToolFact.SignedRef) :
    selectRoot? (.logical logicalId) [⟨logicalId, exact, 2⟩] = none := by
  simp [selectRoot?, rootMatches, RootCandidate.admissibleFor,
    RootCandidate.admissible]

theorem exact_root_remains_selected_with_multiple_current_heads
    (logicalId : LogicalRequestId) (exact : ToolFact.SignedRef)
    (hAuthoritative : exact.authoritative = true) :
    selectRoot? (.exact exact) [⟨logicalId, exact, 2⟩] = some exact := by
  simp [selectRoot?, rootMatches, RootCandidate.admissibleFor, hAuthoritative]

theorem unsigned_root_rejected
    (logicalId docId cid signerDid : Nat) :
    selectRoot? (.logical logicalId)
      [⟨logicalId, ⟨⟨docId, cid⟩, signerDid, false⟩, 1⟩] = none := by
  simp [selectRoot?, rootMatches, RootCandidate.admissibleFor,
    RootCandidate.admissible,
    ToolFact.SignedRef.authoritative]

theorem selected_root_is_authoritative
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {root : ToolFact.SignedRef}
    (hSelected : selectRoot? selector candidates = some root) :
    root.authoritative = true := by
  unfold selectRoot? at hSelected
  generalize candidates.filter (rootMatches selector) = filtered at hSelected
  cases filtered with
  | nil => simp at hSelected
  | cons candidate rest =>
      cases rest with
      | nil =>
          cases hAdmissible : candidate.admissibleFor selector <;>
            simp [hAdmissible] at hSelected
          subst root
          cases selector with
          | exact _ => exact hAdmissible
          | logical _ =>
              have admitted : candidate.currentHeadCount = 1 ∧
                  candidate.exact.authoritative = true := by
                simpa [RootCandidate.admissibleFor, RootCandidate.admissible]
                  using hAdmissible
              exact admitted.2
      | cons _ _ => simp at hSelected

theorem required_slot_omission_rejected
    (slot : SourceSlot) (collection : Nat) (reason : OmissionReason)
    (observed : List ObservedSource) :
    freezeSlots? [⟨slot, .required⟩] observed [.omit slot collection reason] = none := by
  rw [freezeSlots?, decisionFor_single_omit]
  rfl

theorem duplicate_slot_decision_rejected
    (slot : SourceSlot) (collection collectionVersionId : Nat)
    (exact : ToolFact.SignedRef)
    (observed : List ObservedSource) :
    freezeSlots? [⟨slot, .optional⟩] observed
      [.include slot collection collectionVersionId exact,
       .include slot collection collectionVersionId exact] = none := by
  rw [freezeSlots?, decisionFor_duplicate_include]
  rfl

theorem included_source_cid_rebind_rejected
    (slot : SourceSlot) (collection collectionVersionId : Nat)
    (expected current : ToolFact.SignedRef)
    (hExpected : expected.authoritative = true)
    (hDifferent : current ≠ expected) :
    freezeSlots? [⟨slot, .required⟩]
      [⟨slot, collection, collectionVersionId, current⟩]
      [.include slot collection collectionVersionId expected] = none := by
  rw [freezeSlots?, decisionFor_single_include]
  simp [hExpected, observedSource_single, hDifferent]

theorem included_source_collection_rebind_rejected
    (slot : SourceSlot) (expectedCollection currentCollection collectionVersionId : Nat)
    (exact : ToolFact.SignedRef)
    (hExpected : exact.authoritative = true)
    (hDifferent : currentCollection ≠ expectedCollection) :
    freezeSlots? [⟨slot, .required⟩]
      [⟨slot, currentCollection, collectionVersionId, exact⟩]
      [.include slot expectedCollection collectionVersionId exact] = none := by
  rw [freezeSlots?, decisionFor_single_include]
  simp [hExpected, observedSource_single, hDifferent]

theorem included_source_schema_version_rebind_rejected
    (slot : SourceSlot) (collection expectedVersion currentVersion : Nat)
    (exact : ToolFact.SignedRef)
    (hExpected : exact.authoritative = true)
    (hDifferent : currentVersion ≠ expectedVersion) :
    freezeSlots? [⟨slot, .required⟩]
      [⟨slot, collection, currentVersion, exact⟩]
      [.include slot collection expectedVersion exact] = none := by
  rw [freezeSlots?, decisionFor_single_include]
  simp [hExpected, observedSource_single, hDifferent]

theorem optional_omission_is_explicit
    (slot : SourceSlot) (collection : Nat) (reason : OmissionReason)
    (observed : List ObservedSource) :
    freezeSlots? [⟨slot, .optional⟩] observed [.omit slot collection reason] =
      some [.omit slot collection reason] := by
  rw [freezeSlots?, decisionFor_single_omit]
  rfl

theorem accepted_manifest_version_is_two
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    manifest.version = 2 := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      rcases hFreeze with ⟨_, hFreeze⟩
      cases hItems : freezeSlots? expected observed decisions with
      | none => simp [hItems] at hFreeze
      | some items =>
          simp [hItems] at hFreeze
          rcases hFreeze with ⟨_, rfl⟩
          rfl

theorem accepted_manifest_retains_selected_root
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    selectRoot? selector candidates = some manifest.root := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      rcases hFreeze with ⟨_, hFreeze⟩
      cases hItems : freezeSlots? expected observed decisions with
      | none => simp [hItems] at hFreeze
      | some items =>
          simp [hItems] at hFreeze
          rcases hFreeze with ⟨_, rfl⟩
          rfl

theorem accepted_manifest_root_is_authoritative
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    manifest.root.authoritative = true := by
  exact selected_root_is_authoritative (accepted_manifest_retains_selected_root hFreeze)

theorem accepted_manifest_covers_expected_slots
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    manifest.items.map SlotDecision.slot = expected.map (·.slot) := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      rcases hFreeze with ⟨_, hFreeze⟩
      cases hItems : freezeSlots? expected observed decisions with
      | none => simp [hItems] at hFreeze
      | some items =>
          simp [hItems] at hFreeze
          rcases hFreeze with ⟨hChecks, rfl⟩
          exact hChecks.1

theorem accepted_manifest_expected_order_is_canonical
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    slotsStrictlyOrdered (expected.map (·.slot)) = true := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      exact hFreeze.1.1.1.1.2

theorem accepted_manifest_items_are_canonically_ordered
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    slotsStrictlyOrdered (manifest.items.map SlotDecision.slot) = true := by
  rw [accepted_manifest_covers_expected_slots hFreeze]
  exact accepted_manifest_expected_order_is_canonical hFreeze

theorem accepted_manifest_has_exact_observed_membership
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    itemsHaveExactObservedMembership observed manifest.items = true := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      rcases hFreeze with ⟨_, hFreeze⟩
      cases hItems : freezeSlots? expected observed decisions with
      | none => simp [hItems] at hFreeze
      | some items =>
          simp [hItems] at hFreeze
          rcases hFreeze with ⟨hChecks, rfl⟩
          exact hChecks.2

theorem accepted_manifest_has_no_undeclared_decisions
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    decisionsDeclared expected decisions = true := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      exact hFreeze.1.1.1.2

theorem accepted_manifest_has_no_undeclared_observed_sources
    {selector : RootSelector}
    {candidates : List RootCandidate}
    {expected : List ExpectedSlot}
    {observed : List ObservedSource}
    {decisions : List SlotDecision}
    {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    observedDeclared expected observed = true := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      exact hFreeze.1.1.2

theorem accepted_manifest_retains_coverage_gaps
    {selector : RootSelector} {candidates : List RootCandidate}
    {expected : List ExpectedSlot} {observed : List ObservedSource}
    {decisions : List SlotDecision} {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    manifest.coverageGaps = coverageGaps := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      rcases hFreeze with ⟨_, hFreeze⟩
      cases hItems : freezeSlots? expected observed decisions with
      | none => simp [hItems] at hFreeze
      | some items =>
          simp [hItems] at hFreeze
          rcases hFreeze with ⟨_, rfl⟩
          rfl

theorem accepted_manifest_status_matches_gaps
    {selector : RootSelector} {candidates : List RootCandidate}
    {expected : List ExpectedSlot} {observed : List ObservedSource}
    {decisions : List SlotDecision} {coverageGaps : List CoverageGap}
    {manifest : Manifest}
    (hFreeze : freeze? selector candidates expected observed decisions coverageGaps = some manifest) :
    manifest.status = statusFor coverageGaps manifest.items := by
  unfold freeze? at hFreeze
  cases hRoot : selectRoot? selector candidates with
  | none => simp [hRoot] at hFreeze
  | some root =>
      simp [hRoot] at hFreeze
      rcases hFreeze with ⟨_, hFreeze⟩
      cases hItems : freezeSlots? expected observed decisions with
      | none => simp [hItems] at hFreeze
      | some items =>
          simp [hItems] at hFreeze
          rcases hFreeze with ⟨_, rfl⟩
          rfl

theorem empty_gaps_and_no_omissions_are_verified_exact
    (items : List SlotDecision) (hNoOmissions : itemsHaveNoOmissions items = true) :
    statusFor [] items = .verifiedExact := by
  simp [statusFor, hNoOmissions]

theorem empty_gaps_with_omission_are_partial_exact
    (slot : SourceSlot) (collection : Nat) (reason : OmissionReason)
    (rest : List SlotDecision) :
    statusFor [] (.omit slot collection reason :: rest) = .partialExact := by
  rfl

theorem nonempty_gaps_are_partial_exact
    (gap : CoverageGap) (rest : List CoverageGap) (items : List SlotDecision) :
    statusFor (gap :: rest) items = .partialExact := by
  rfl

theorem duplicate_gaps_are_not_canonical (gap : CoverageGap) :
    gapsStrictlyOrdered [gap, gap] = false := by
  simp [gapsStrictlyOrdered, CoverageGap.lt]

theorem noncanonical_coverage_gaps_rejected
    (selector : RootSelector) (candidates : List RootCandidate)
    (expected : List ExpectedSlot) (observed : List ObservedSource)
    (decisions : List SlotDecision) (coverageGaps : List CoverageGap)
    (hNoncanonical : gapsStrictlyOrdered coverageGaps = false) :
    freeze? selector candidates expected observed decisions coverageGaps = none := by
  unfold freeze?
  cases hRoot : selectRoot? selector candidates <;> simp [hRoot, hNoncanonical]

theorem duplicate_coverage_gaps_rejected
    (selector : RootSelector) (candidates : List RootCandidate)
    (expected : List ExpectedSlot) (observed : List ObservedSource)
    (decisions : List SlotDecision) (gap : CoverageGap) :
    freeze? selector candidates expected observed decisions [gap, gap] = none := by
  apply noncanonical_coverage_gaps_rejected
  exact duplicate_gaps_are_not_canonical gap

/-!
## Provenance-edge closure

Some timeline facts contain exact references nested inside their payloads. A
`RenderedRequest`, for example, pins transcript and resolved-configuration
facts in its provenance manifest. Parsing the outer row is not enough: each
declared edge must become an included source with the same collection, schema
version, document id, composite commit, and verified signer.

The store adapter recursively discovers and cryptographically verifies those
edges before supplying `DeclaredExactEdge`s here. This wrapper makes omission
or rebinding of any discovered edge a rejected freeze rather than an
apparently complete manifest.
-/

structure DeclaredExactEdge where
  collection : Nat
  collectionVersionId : Nat
  exact : ToolFact.SignedRef
  deriving DecidableEq, Repr

def SlotDecision.coversDeclaredEdge
    (decision : SlotDecision) (edge : DeclaredExactEdge) : Bool :=
  match decision with
  | .include _ collection collectionVersionId exact =>
      collection == edge.collection
        && collectionVersionId == edge.collectionVersionId
        && decide (exact = edge.exact)
  | .omit _ _ _ => false

def declaredEdgesCovered
    (edges : List DeclaredExactEdge) (items : List SlotDecision) : Bool :=
  edges.all fun edge => items.any (·.coversDeclaredEdge edge)

def freezeWithDeclaredEdges?
    (selector : RootSelector)
    (candidates : List RootCandidate)
    (expected : List ExpectedSlot)
    (observed : List ObservedSource)
    (decisions : List SlotDecision)
    (coverageGaps : List CoverageGap)
    (declaredEdges : List DeclaredExactEdge) : Option Manifest := do
  let manifest ← freeze? selector candidates expected observed decisions coverageGaps
  if declaredEdgesCovered declaredEdges manifest.items then some manifest else none

theorem accepted_manifest_covers_every_declared_exact_edge
    {selector : RootSelector} {candidates : List RootCandidate}
    {expected : List ExpectedSlot} {observed : List ObservedSource}
    {decisions : List SlotDecision} {coverageGaps : List CoverageGap}
    {declaredEdges : List DeclaredExactEdge} {manifest : Manifest}
    (hFreeze : freezeWithDeclaredEdges? selector candidates expected observed decisions
      coverageGaps declaredEdges = some manifest) :
    declaredEdgesCovered declaredEdges manifest.items = true := by
  unfold freezeWithDeclaredEdges? at hFreeze
  cases hBase : freeze? selector candidates expected observed decisions coverageGaps with
  | none => simp [hBase] at hFreeze
  | some base =>
      simp [hBase] at hFreeze
      rcases hFreeze with ⟨hCovered, rfl⟩
      exact hCovered

@[simp] theorem missing_declared_exact_edge_is_not_covered (edge : DeclaredExactEdge) :
    declaredEdgesCovered [edge] [] = false := by
  rfl

theorem rebound_declared_exact_edge_is_not_covered
    (slot : SourceSlot) (declared rebound : DeclaredExactEdge)
    (hDifferent : declared ≠ rebound) :
    declaredEdgesCovered [declared]
      [.include slot rebound.collection rebound.collectionVersionId rebound.exact] = false := by
  simp [declaredEdgesCovered, SlotDecision.coversDeclaredEdge]
  intro hCollection hVersion hExact
  apply hDifferent
  cases declared
  cases rebound
  simp_all

end RunTimelineManifest
