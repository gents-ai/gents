import Proofs.InferenceCall.Persistence
import Proofs.CanonicalOutput.Execution.State
import Proofs.PromptAssembly.AggregateBudget
import Mathlib.Algebra.BigOperators.Group.Finset.Basic

/-!
# Request-scoped inference usage projections

`InferenceCall.Persistence.Row.usage` remains the one durable provider usage
observation, including observations arriving after its terminal write. The
finite call-ID set represents physical `InferenceCall` identity: repeating an
identical row in a query cannot add that ID a second time. A coherent catalog
cannot contain different rows for one call ID. The translated row carries the
call's physical `request_doc_id` alongside the persisted call, whose
`requestId` represents only the logical ID. Historical signed title provenance
remains observable after the active claim is cleared.
-/

open scoped BigOperators

namespace InferenceCall.UsageProjection

abbrev Usage := PromptAssembly.AggregateBudget.PersistedUsage
abbrev TitleBinding := CanonicalOutput.Execution.Handover.TitleBinding

/-- The native call row's `request_doc_id` is the physical request document,
not the logical `InferenceCall.requestId`. Translation must retain both. -/
structure UsageRow where
  physicalRequest : CanonicalOutput.DocId
  persisted : Persistence.Row Usage

/-- An exact normal parent query, including its durable request state. State is
an observation, never an authority to suppress later child usage. -/
structure ParentScope where
  physical : CanonicalOutput.DocId
  logical : RequestId
  agent : Nat
  session : SessionId
  state : RequestState

/-- Call IDs, not repeated query rows, define the accounting domain. `purpose`
comes from the required signed request field; `titleBinding` is authenticated
title provenance, not an assertion of historical execution or a caller-supplied
parent label. The execution claim fence remains an independent write authority. -/
structure Observation where
  callIds : Finset Nat
  row : Nat → UsageRow
  purpose : CanonicalOutput.DocId → Option RequestPurpose
  titleBinding : CanonicalOutput.DocId → Option TitleBinding

/-- A native query may repeat an identical call row, but cannot choose a first
winner for conflicting rows under one physical call ID. The decoder must
reject a query for which this predicate cannot be established. -/
structure CatalogCoherent (records : List (Nat × UsageRow))
    (observation : Observation) : Prop where
  ids : observation.callIds = (records.map Prod.fst).toFinset
  exact : ∀ callId row, (callId, row) ∈ records → row = observation.row callId
  identity : ∀ callId row, (callId, row) ∈ records →
    row.persisted.call.callId = callId

theorem coherent_catalog_rejects_conflicting_rows (records : List (Nat × UsageRow))
    (observation : Observation) (coherent : CatalogCoherent records observation)
    (callId : Nat) (left right : UsageRow)
    (leftPresent : (callId, left) ∈ records)
    (rightPresent : (callId, right) ∈ records) : left = right := by
  exact (coherent.exact callId left leftPresent).trans
    (coherent.exact callId right rightPresent).symm

def rowUsage (row : UsageRow) : Usage :=
  row.persisted.usage.getD { promptTokens := 0, completionTokens := 0 }

def sumWhere (observation : Observation) (eligible : UsageRow → Bool) : Nat × Nat :=
  (∑ callId ∈ observation.callIds,
      if eligible (observation.row callId) then
        (rowUsage (observation.row callId)).promptTokens else 0,
    ∑ callId ∈ observation.callIds,
      if eligible (observation.row callId) then
        (rowUsage (observation.row callId)).completionTokens else 0)

/-- The ordinary public projection selects only exact, signed-normal request
IDs. Title calls never become public merely by carrying parent provenance. -/
def normalPublic (observation : Observation) (parent : ParentScope) : Nat × Nat :=
  sumWhere observation fun row =>
    decide (observation.purpose parent.physical = some .normal ∧
      row.persisted.call.requestId = parent.logical ∧
      row.physicalRequest = parent.physical)

/-- Validate authenticated title provenance against the call's own immutable
physical and logical request identities. An unrelated parent is not a malformed
self-binding; execution history is not inferred from this provenance. -/
def titleSelfBindingValid (row : UsageRow) (binding : TitleBinding) : Bool :=
  decide (binding.authenticated = true ∧
    row.persisted.call.requestId = binding.logicalRequest ∧
    row.physicalRequest = binding.physicalRequest)

/-- Join one valid title call through authenticated title parent provenance.
Matching the selected parent is projection membership, not authentication. -/
def titleBelongsTo (observation : Observation) (parent : ParentScope)
    (row : UsageRow) : Bool :=
  match observation.titleBinding row.physicalRequest with
  | some binding =>
      titleSelfBindingValid row binding &&
        decide (binding.parentLogical = parent.logical ∧
          binding.parentPhysical = parent.physical ∧
          binding.agent = parent.agent ∧
          binding.session = parent.session)
  | none => false

def titleChildren (observation : Observation) (parent : ParentScope) : Nat × Nat :=
  sumWhere observation fun row =>
    decide (observation.purpose row.physicalRequest = some .titleAudit) &&
      titleBelongsTo observation parent row

/-- Explicit audit projection, never the ordinary public request total. The
same unique physical call contributes its persisted components once. -/
def parentInclusiveAudit (observation : Observation) (parent : ParentScope) : Nat × Nat :=
  let normal := normalPublic observation parent
  let titles := titleChildren observation parent
  (normal.1 + titles.1, normal.2 + titles.2)

inductive CatalogError where
  | mismatchedCallId
  | conflictingCallId
  | missingPurpose
  | missingTitleBinding
  | invalidTitleBinding
  deriving DecidableEq, Repr

/-- Exact persisted-row comparison for one physical call ID. No field is chosen
from the first row when two native query observations disagree. -/
def sameUsageRow (left right : UsageRow) : Bool :=
  decide (left.physicalRequest = right.physicalRequest ∧
    left.persisted.call.callId = right.persisted.call.callId ∧
    left.persisted.call.requestId = right.persisted.call.requestId ∧
    left.persisted.call.backend = right.persisted.call.backend ∧
    left.persisted.call.state = right.persisted.call.state ∧
    left.persisted.terminalStamp = right.persisted.terminalStamp ∧
    left.persisted.usage = right.persisted.usage)

theorem sameUsageRow_sound (left right : UsageRow)
    (same : sameUsageRow left right = true) : left = right := by
  simp only [sameUsageRow, decide_eq_true_eq] at same
  rcases same with ⟨hphysical, hcallId, hrequestId, hbackend, hstate, hstamp, husage⟩
  cases left with
  | mk leftPhysical leftPersisted =>
    cases right with
    | mk rightPhysical rightPersisted =>
      cases leftPersisted with
      | mk leftCall leftStamp leftUsage =>
        cases rightPersisted with
        | mk rightCall rightStamp rightUsage =>
          cases leftCall with
          | mk leftId leftRequest leftBackend leftState =>
            cases rightCall with
            | mk rightId rightRequest rightBackend rightState =>
              cases hphysical
              cases hcallId
              cases hrequestId
              cases hbackend
              cases hstate
              cases hstamp
              cases husage
              rfl

/-- Executable finite check of the catalog premise. The projection gate calls
this on the original raw records, not merely on the normalized representatives. -/
def coherentCheck (records : List (Nat × UsageRow))
    (observation : Observation) : Bool :=
  decide (observation.callIds = (records.map Prod.fst).toFinset) &&
    records.all (fun (callId, row) => sameUsageRow row (observation.row callId)) &&
    records.all (fun (callId, row) => decide (row.persisted.call.callId = callId))

theorem coherentCheck_sound (records : List (Nat × UsageRow))
    (observation : Observation) (checked : coherentCheck records observation = true) :
    CatalogCoherent records observation := by
  simp only [coherentCheck, Bool.and_eq_true, decide_eq_true_eq] at checked
  rcases checked with ⟨⟨ids, exactRows⟩, identities⟩
  refine ⟨ids, ?_, ?_⟩
  · intro callId row present
    exact sameUsageRow_sound row (observation.row callId)
      ((List.all_eq_true.mp exactRows) (callId, row) present)
  · intro callId row present
    exact of_decide_eq_true ((List.all_eq_true.mp identities) (callId, row) present)

def validateCatalog : List (Nat × UsageRow) →
    Except CatalogError (List (Nat × UsageRow))
  | [] => .ok []
  | (callId, row) :: rest => do
      if row.persisted.call.callId != callId then
        throw .mismatchedCallId
      let unique ← validateCatalog rest
      match unique.find? (fun entry => entry.1 == callId) with
      | none => pure ((callId, row) :: unique)
      | some (_, old) =>
          if sameUsageRow row old then pure unique
          else throw .conflictingCallId

private def uniqueObservation (first : Nat × UsageRow)
    (rest : List (Nat × UsageRow))
    (purpose : CanonicalOutput.DocId → Option RequestPurpose)
    (titleBinding : CanonicalOutput.DocId → Option TitleBinding) : Observation :=
  let rows := first :: rest
  { callIds := (rows.map Prod.fst).toFinset
  , row := fun callId =>
      ((rows.find? (fun entry => entry.1 == callId)).map Prod.snd).getD first.2
  , purpose, titleBinding }

structure Totals where
  normalPublic : Nat × Nat
  parentInclusiveAudit : Nat × Nat
  deriving DecidableEq, Repr

/-- Native ingestion of bounded candidate calls rejects conflicting rows and
unresolved signed purpose/title-provenance facts before returning numeric totals. An
empty catalog has zero totals; nonempty projections reuse the same finite-ID
owner as the verified single-row observation. Candidate scope and authenticated
physical facts remain native premises. -/
def validateAndProject (records : List (Nat × UsageRow))
    (purpose : CanonicalOutput.DocId → Option RequestPurpose)
    (titleBinding : CanonicalOutput.DocId → Option TitleBinding)
    (parent : ParentScope) : Except CatalogError Totals := do
  let unique ← validateCatalog records
  if records.any (fun (_, row) => (purpose row.physicalRequest).isNone) then
    throw .missingPurpose
  if records.any (fun (_, row) =>
      decide (purpose row.physicalRequest = some .titleAudit) &&
        (titleBinding row.physicalRequest).isNone) then
    throw .missingTitleBinding
  if records.any (fun (_, row) =>
      decide (purpose row.physicalRequest = some .titleAudit) &&
        match titleBinding row.physicalRequest with
        | some binding => !titleSelfBindingValid row binding
        | none => false) then
    throw .invalidTitleBinding
  match unique with
  | [] => pure ⟨(0, 0), (0, 0)⟩
  | first :: rest =>
      let observation := uniqueObservation first rest purpose titleBinding
      if coherentCheck records observation then
        pure ⟨normalPublic observation parent, parentInclusiveAudit observation parent⟩
      else throw .conflictingCallId

theorem parent_inclusive_decomposes (observation : Observation) (parent : ParentScope) :
    parentInclusiveAudit observation parent =
      ((normalPublic observation parent).1 + (titleChildren observation parent).1,
       (normalPublic observation parent).2 + (titleChildren observation parent).2) := by
  rfl

theorem duplicate_call_id_does_not_double_count (observation : Observation)
    (parent : ParentScope) (callId : Nat) (present : callId ∈ observation.callIds) :
    normalPublic { observation with callIds := insert callId observation.callIds } parent =
      normalPublic observation parent ∧
    parentInclusiveAudit { observation with callIds := insert callId observation.callIds } parent =
      parentInclusiveAudit observation parent := by
  simp [Finset.insert_eq_of_mem present]

/-- A parent may have terminalized before the title provider reports usage;
neither projection filters on the parent's lifecycle state. -/
theorem parent_terminal_does_not_drop_late_usage (observation : Observation)
    (parent : ParentScope) (terminal : RequestState) :
    parentInclusiveAudit observation { parent with state := terminal } =
      parentInclusiveAudit observation parent := by
  rfl

/-- Late usage changes the persisted usage projection without altering the
terminal call or terminal timestamp already selected by the call writer. -/
theorem observe_usage_after_terminal (row : UsageRow) (usage : Usage) :
    rowUsage { row with persisted := Persistence.observeUsage row.persisted usage } = usage ∧
    (Persistence.observeUsage row.persisted usage).call = row.persisted.call ∧
    (Persistence.observeUsage row.persisted usage).terminalStamp =
      row.persisted.terminalStamp := by
  exact ⟨rfl, rfl, rfl⟩

private def witnessParent : ParentScope :=
  { physical := 10, logical := 20, agent := 30, session := 40, state := .completed }

private def witnessRow (physical : CanonicalOutput.DocId) (logical : RequestId)
    (usage : Option Usage) : UsageRow :=
  { physicalRequest := physical
  , persisted :=
      { call :=
          { callId := 7
          , requestId := logical
          , backend := { val := "backend" }
          , state := .completed }
      , terminalStamp := some 99
      , usage := usage } }

private def witnessTitleBinding : CanonicalOutput.Execution.Handover.TitleBinding :=
  { physicalRequest := 11, logicalRequest := 21,
    parentPhysical := 10, parentLogical := 20,
    agent := 30, session := 40, authenticated := true }

private def witnessObservation (row : UsageRow) : Observation :=
  { callIds := {7}
  , row := fun _ => row
  , purpose := fun physical => if physical = 11 then some .titleAudit else some .normal
  , titleBinding := fun physical => if physical = 11 then some witnessTitleBinding else none }

private def witnessUsage : Usage :=
  { promptTokens := 3, completionTokens := 5 }

theorem title_usage_excluded_from_public :
    normalPublic (witnessObservation (witnessRow 11 21 (some witnessUsage)))
      witnessParent = (0, 0) := by
  native_decide

theorem title_usage_in_parent_audit :
    parentInclusiveAudit (witnessObservation (witnessRow 11 21 (some witnessUsage)))
      witnessParent = (3, 5) := by
  native_decide

theorem repeated_exact_title_call_counts_once :
    let observation := witnessObservation (witnessRow 11 21 (some witnessUsage))
    parentInclusiveAudit { observation with callIds := insert 7 observation.callIds }
      witnessParent = (3, 5) := by
  native_decide

theorem same_logical_wrong_physical_excluded :
    parentInclusiveAudit (witnessObservation (witnessRow 12 21 (some witnessUsage)))
      witnessParent = (0, 0) := by
  native_decide

theorem normal_same_logical_wrong_physical_excluded :
    normalPublic (witnessObservation (witnessRow 12 20 (some witnessUsage)))
      witnessParent = (0, 0) := by
  native_decide

theorem late_title_usage_after_parent_terminal :
    let row := witnessRow 11 21 none
    let observed := { row with persisted := Persistence.observeUsage row.persisted witnessUsage }
    parentInclusiveAudit (witnessObservation row) witnessParent = (0, 0) ∧
      parentInclusiveAudit (witnessObservation observed) witnessParent = (3, 5) := by
  native_decide

end InferenceCall.UsageProjection
