import Proofs.InferenceCall.Persistence
import Proofs.CanonicalOutput.Execution.State
import Proofs.PromptAssembly.AggregateBudget
import Mathlib.Algebra.BigOperators.Group.Finset.Basic

open scoped BigOperators

namespace InferenceCall.UsageProjection

abbrev Usage := PromptAssembly.AggregateBudget.PersistedUsage
abbrev AuxiliaryBinding := CanonicalOutput.Execution.Handover.AuxiliaryBinding

structure UsageRow where
  physicalRequest : CanonicalOutput.DocId
  persisted : Persistence.Row Usage

structure ParentScope where
  physical : CanonicalOutput.DocId
  logical : RequestId
  node : Nat
  session : SessionId
  state : RequestState

structure Observation where
  callIds : Finset Nat
  row : Nat → UsageRow
  purpose : CanonicalOutput.DocId → Option RequestPurpose
  auxiliaryBinding : CanonicalOutput.DocId → Option AuxiliaryBinding

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

def normalBelongsTo (observation : Observation) (parent : ParentScope)
    (row : UsageRow) : Bool :=
  decide (observation.purpose parent.physical = some .normal ∧
    row.persisted.call.requestId = parent.logical ∧
    row.physicalRequest = parent.physical)

def normalPublic (observation : Observation) (parent : ParentScope) : Nat × Nat :=
  sumWhere observation (normalBelongsTo observation parent)

def auxiliarySelfBindingValid (row : UsageRow) (binding : AuxiliaryBinding) : Bool :=
  decide (binding.purpose.isAuxiliary = true ∧ binding.authenticated = true ∧
    row.persisted.call.requestId = binding.logicalRequest ∧
    row.physicalRequest = binding.physicalRequest)

def auxiliaryBelongsTo (observation : Observation) (parent : ParentScope)
    (row : UsageRow) : Bool :=
  match observation.auxiliaryBinding row.physicalRequest with
  | some binding =>
      auxiliarySelfBindingValid row binding &&
        decide (observation.purpose row.physicalRequest = some binding.purpose) &&
        decide (binding.parentLogical = parent.logical ∧
          binding.parentPhysical = parent.physical ∧
          binding.node = parent.node ∧
          binding.session = parent.session)
  | none => false

def auxiliaryChildren (observation : Observation) (parent : ParentScope) : Nat × Nat :=
  sumWhere observation fun row =>
    (observation.purpose row.physicalRequest).any RequestPurpose.isAuxiliary &&
      auxiliaryBelongsTo observation parent row

def parentInclusiveAudit (observation : Observation) (parent : ParentScope) : Nat × Nat :=
  let normal := normalPublic observation parent
  let titles := auxiliaryChildren observation parent
  (normal.1 + titles.1, normal.2 + titles.2)

/-- Access review is part of carrying out a user's work and consumes its goal
budget. Session title generation is presentation overhead and remains audit-only.
An auxiliary call is chargeable only through its authenticated physical parent. -/
def goalBelongsTo (observation : Observation) (parent : ParentScope)
    (row : UsageRow) : Bool :=
  normalBelongsTo observation parent row ||
    (observation.purpose row.physicalRequest == some .accessReview &&
      auxiliaryBelongsTo observation parent row)

def goalUsage (observation : Observation) (parent : ParentScope) : Nat × Nat :=
  sumWhere observation (goalBelongsTo observation parent)

theorem a_title_cannot_enter_a_goal_budget (observation : Observation)
    (parent : ParentScope) (row : UsageRow)
    (title : observation.purpose row.physicalRequest = some .titleAudit) :
    goalBelongsTo observation parent row = false := by
  simp only [goalBelongsTo, normalBelongsTo]
  by_cases same : row.physicalRequest = parent.physical
  · simp [same] at title ⊢
    simp [title]
  · simp [same, title]

theorem a_review_without_a_parent_cannot_enter_a_goal_budget
    (observation : Observation) (parent : ParentScope) (row : UsageRow)
    (review : observation.purpose row.physicalRequest = some .accessReview)
    (unbound : observation.auxiliaryBinding row.physicalRequest = none) :
    goalBelongsTo observation parent row = false := by
  simp only [goalBelongsTo, normalBelongsTo, auxiliaryBelongsTo, unbound]
  by_cases same : row.physicalRequest = parent.physical
  · simp [same] at review ⊢
    simp [review]
  · simp [same]

inductive CatalogError where
  | mismatchedCallId
  | conflictingCallId
  | missingPurpose
  | missingAuxiliaryBinding
  | invalidAuxiliaryBinding
  deriving DecidableEq, Repr

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
    (auxiliaryBinding : CanonicalOutput.DocId → Option AuxiliaryBinding) : Observation :=
  let rows := first :: rest
  { callIds := (rows.map Prod.fst).toFinset
  , row := fun callId =>
      ((rows.find? (fun entry => entry.1 == callId)).map Prod.snd).getD first.2
  , purpose, auxiliaryBinding }

structure Totals where
  normalPublic : Nat × Nat
  parentInclusiveAudit : Nat × Nat
  deriving DecidableEq, Repr

def validateAndProject (records : List (Nat × UsageRow))
    (purpose : CanonicalOutput.DocId → Option RequestPurpose)
    (auxiliaryBinding : CanonicalOutput.DocId → Option AuxiliaryBinding)
    (parent : ParentScope) : Except CatalogError Totals := do
  let unique ← validateCatalog records
  if records.any (fun (_, row) => (purpose row.physicalRequest).isNone) then
    throw .missingPurpose
  if records.any (fun (_, row) =>
      (purpose row.physicalRequest).any RequestPurpose.isAuxiliary &&
        (auxiliaryBinding row.physicalRequest).isNone) then
    throw .missingAuxiliaryBinding
  if records.any (fun (_, row) =>
      (purpose row.physicalRequest).any RequestPurpose.isAuxiliary &&
        match auxiliaryBinding row.physicalRequest with
        | some binding => !auxiliarySelfBindingValid row binding ||
            purpose row.physicalRequest != some binding.purpose
        | none => false) then
    throw .invalidAuxiliaryBinding
  match unique with
  | [] => pure ⟨(0, 0), (0, 0)⟩
  | first :: rest =>
      let observation := uniqueObservation first rest purpose auxiliaryBinding
      if coherentCheck records observation then
        pure ⟨normalPublic observation parent, parentInclusiveAudit observation parent⟩
      else throw .conflictingCallId

inductive BudgetError where
  | catalog (error : CatalogError)
  | unreportedUsage
  deriving DecidableEq, Repr

def projectGoal (records : List (Nat × UsageRow))
    (purpose : CanonicalOutput.DocId → Option RequestPurpose)
    (auxiliaryBinding : CanonicalOutput.DocId → Option AuxiliaryBinding)
    (parent : ParentScope) : Except CatalogError Nat := do
  let _ ← validateAndProject records purpose auxiliaryBinding parent
  let unique ← validateCatalog records
  match unique with
  | [] => pure 0
  | first :: rest =>
      let usage := goalUsage (uniqueObservation first rest purpose auxiliaryBinding) parent
      pure (usage.1 + usage.2)

/-- Restart uses the authenticated parent-inclusive catalog. A missing or zero
usage report cannot establish a remaining provider allowance, even for a failed
or cancelled call. Late positive usage can make that allowance observable again;
the parent terminal state never discards paid review work. -/
def recoverBudget (records : List (Nat × UsageRow))
    (purpose : CanonicalOutput.DocId → Option RequestPurpose)
    (auxiliaryBinding : CanonicalOutput.DocId → Option AuxiliaryBinding)
    (parent : ParentScope) : Except BudgetError Nat := do
  let totals ← (validateAndProject records purpose auxiliaryBinding parent).mapError .catalog
  let unique ← (validateCatalog records).mapError .catalog
  match unique with
  | [] => pure 0
  | first :: rest =>
      let observation := uniqueObservation first rest purpose auxiliaryBinding
      if unique.any (fun (_, row) =>
          (normalBelongsTo observation parent row || auxiliaryBelongsTo observation parent row) &&
          !(row.persisted.usage.any (fun usage => decide (0 < usage.chargedTotal)))) then
        throw .unreportedUsage
      pure (totals.parentInclusiveAudit.1 + totals.parentInclusiveAudit.2)

theorem parent_inclusive_decomposes (observation : Observation) (parent : ParentScope) :
    parentInclusiveAudit observation parent =
      ((normalPublic observation parent).1 + (auxiliaryChildren observation parent).1,
       (normalPublic observation parent).2 + (auxiliaryChildren observation parent).2) := by
  rfl

theorem duplicate_call_id_does_not_double_count (observation : Observation)
    (parent : ParentScope) (callId : Nat) (present : callId ∈ observation.callIds) :
    normalPublic { observation with callIds := insert callId observation.callIds } parent =
      normalPublic observation parent ∧
    parentInclusiveAudit { observation with callIds := insert callId observation.callIds } parent =
      parentInclusiveAudit observation parent := by
  simp [Finset.insert_eq_of_mem present]

theorem parent_terminal_does_not_drop_late_usage (observation : Observation)
    (parent : ParentScope) (terminal : RequestState) :
    parentInclusiveAudit observation { parent with state := terminal } =
      parentInclusiveAudit observation parent := by
  rfl

theorem parent_terminal_preserves_recoverable_budget (records : List (Nat × UsageRow))
    (purpose : CanonicalOutput.DocId → Option RequestPurpose)
    (auxiliaryBinding : CanonicalOutput.DocId → Option AuxiliaryBinding)
    (parent : ParentScope) (terminal : RequestState) :
    recoverBudget records purpose auxiliaryBinding { parent with state := terminal } =
      recoverBudget records purpose auxiliaryBinding parent := by
  rfl

theorem observe_usage_after_terminal (row : UsageRow) (usage : Usage) :
    rowUsage { row with persisted := Persistence.observeUsage row.persisted usage } = usage ∧
    (Persistence.observeUsage row.persisted usage).call = row.persisted.call ∧
    (Persistence.observeUsage row.persisted usage).terminalStamp =
      row.persisted.terminalStamp := by
  exact ⟨rfl, rfl, rfl⟩

private def witnessParent : ParentScope :=
  { physical := 10, logical := 20, node := 30, session := 40, state := .completed }

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

private def witnessAuxiliaryBinding : CanonicalOutput.Execution.Handover.AuxiliaryBinding :=
  { physicalRequest := 11, logicalRequest := 21,
    parentPhysical := 10, parentLogical := 20,
    node := 30, session := 40, authenticated := true }

private def witnessObservation (row : UsageRow) : Observation :=
  { callIds := {7}
  , row := fun _ => row
  , purpose := fun physical => if physical = 11 then some .titleAudit else some .normal
  , auxiliaryBinding := fun physical => if physical = 11 then some witnessAuxiliaryBinding else none }

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
