import Proofs.TitleAdmission
import Proofs.Conformance.ContractCases.Enrollment
import Proofs.Compaction.State
import Lean

namespace Conformance.TitleAdmission

open Lean CanonicalOutput.Execution

private def ids : _root_.TitleAdmission.Identities :=
  { encode := Compaction.PromptView.encodeProviderKey
  , injective := Compaction.PromptView.encodeProviderKey_injective }

private def request := Conformance.ContractCases.titleRequest
private def admission := Conformance.ContractCases.signTitleAdmission request
private def evidence : Option Enrollment.RuntimeInternalEvidence :=
  some Conformance.ContractCases.titleEvidence

private def baseRow : _root_.TitleAdmission.RequestRowEvidence :=
  { physicalRequest := "T"
  , logicalRequest := request.requestId
  , signedFields := Enrollment.agentRequestAdmissionFields request admission
  , physicalBindingCurrent := true
  , branchFieldsExact := true
  , pendingDeadlineAbsent := true }

private structure Case where
  name : String
  available : Bool := true
  row : _root_.TitleAdmission.RequestRowEvidence := baseRow
  ownPhysical : String := "T"
  activeNormal : Bool := false

private def admittedCase : Case :=
  { name := "admitted-pending-title-claims-own-lease" }
private def activeQueueCase : Case :=
  { name := "admitted-title-while-normal-queue-active", activeNormal := true }
private def wrongWorldPhysicalCase : Case :=
  { name := "own-physical-world-mismatch", ownPhysical := "X" }
private def unavailableCase : Case :=
  { name := "observation-unavailable", available := false }

private def cases : List Case :=
  [ admittedCase
  , activeQueueCase
  , { name := "row-logical-request-mismatch"
    , row := { baseRow with logicalRequest := "x" } }
  , { name := "row-signed-fields-mismatch"
    , row := { baseRow with signedFields := [] } }
  , { name := "row-physical-binding-stale"
    , row := { baseRow with physicalBindingCurrent := false } }
  , wrongWorldPhysicalCase
  , unavailableCase
  , { name := "foreign-branch-fields"
    , row := { baseRow with branchFieldsExact := false } }
  , { name := "present-preclaim-deadline"
    , row := { baseRow with pendingDeadlineAbsent := false } } ]

private def world (c : Case) : World :=
  let session := ids.encode request.sessionId
  let agent := ids.encode request.targetAgent
  let active := if c.activeNormal then some (ids.encode "n") else none
  let queue : SessionQueue.SessionQueueState :=
    { scope := { agent := agent, session := session, requester := none }, active := active, pending := [], terminal := ∅ }
  { requestId := ids.encode c.ownPhysical
  , sessionId := session
  , purpose := request.purpose
  , principal := agent
  , lease := RequestExecutionLease.initial Nat
  , segments := []
  , messages := []
  , transcript := { sessionId := session, nextSeq := 0, messages := [], toolCalls := [], inFlight := ∅ }
  , terminalSelection := none
  , gateOwner := some 9
  , gateSchedule := ⟨.storage, true, false⟩
  , queue := queue }

private def activation (c : Case) : Option Handover.TitleActivation :=
  _root_.TitleAdmission.activation? ids c.available ({} : Enrollment.State)
    request admission evidence request.behaviorId c.row 7 20 30

private def claimed (c : Case) : Option World :=
  _root_.TitleAdmission.activate? (world c) 9 1 ids c.available
    ({} : Enrollment.State) request admission evidence request.behaviorId
    c.row 7 20 30 99
    { transportRetries := 0, resampleRetries := 0, allowRepair := false } none

private theorem admitted_claims_live_lease :
    (claimed admittedCase).isSome = true ∧
      (match (claimed admittedCase).map (·.lease.lease) with
       | some (.active _ _ _) => true
       | _ => false) = true := by
  native_decide

private theorem active_normal_queue_is_not_consumed :
    (claimed activeQueueCase).isSome = true ∧
      (claimed activeQueueCase).map (·.queue) = some (world activeQueueCase).queue := by
  native_decide

private theorem wrong_world_physical_rejects_valid_activation :
    (activation wrongWorldPhysicalCase).isSome = true ∧
      (claimed wrongWorldPhysicalCase).isNone = true := by
  native_decide

private theorem unavailable_observation_retries_without_claim :
    Enrollment.titlePendingDisposition false ({} : Enrollment.State) request admission
      evidence request.behaviorId unavailableCase.row.branchFieldsExact
      unavailableCase.row.pendingDeadlineAbsent = .retry ∧
      (claimed unavailableCase).isNone = true := by
  native_decide

/-- This bounds only the decimal fixture symbols emitted below, not request
identity or the native adapter's supported ID width. -/
private def emittedModelLabels (c : Case) : List Nat :=
  let start := world c
  let binding := (activation c).map fun activation =>
    let value := activation.binding
    [value.physicalRequest, value.logicalRequest, value.parentPhysical,
     value.parentLogical, value.agent, value.session]
  [start.requestId, start.sessionId, start.principal,
   start.queue.scope.agent, start.queue.scope.session,
   start.queue.active.getD 0] ++ binding.getD []

private theorem emitted_symbol_ids_remain_bounded :
    cases.all (fun c => (emittedModelLabels c).all (· < 65536)) = true := by
  native_decide

private def rowJson (r : _root_.TitleAdmission.RequestRowEvidence) : Json :=
  Json.mkObj
    [ ("physical_request", toJson r.physicalRequest)
    , ("logical_request", toJson r.logicalRequest)
    , ("model_signed_fields_hex", toJson (r.signedFields.map Enrollment.utf8HexString))
    , ("physical_binding_current", toJson r.physicalBindingCurrent)
    , ("branch_fields_exact", toJson r.branchFieldsExact)
    , ("pending_deadline_absent", toJson r.pendingDeadlineAbsent) ]

/-- Collision-free model labels may exceed native integer widths; decimal JSON
strings preserve exact equality and are not durable document identifiers. -/
private def symbolJson (value : Nat) : Json := toJson (toString value)
private def optionalSymbolJson (value : Option Nat) : Json :=
  value.map symbolJson |>.getD Json.null

private def bindingJson (b : Handover.TitleBinding) : Json :=
  Json.mkObj
    [ ("physical_request", symbolJson b.physicalRequest)
    , ("logical_request", symbolJson b.logicalRequest)
    , ("parent_physical", symbolJson b.parentPhysical)
    , ("parent_logical", symbolJson b.parentLogical)
    , ("agent", symbolJson b.agent)
    , ("session", symbolJson b.session)
    , ("authenticated", toJson b.authenticated) ]

private def caseJson (c : Case) : Json :=
  let start := world c
  let decision := activation c
  let result := claimed c
  Json.mkObj
    [ ("name", toJson c.name)
    , ("admission_case", toJson "valid-title-parent-completed")
    , ("observation_available", toJson c.available)
    , ("row", rowJson c.row)
    , ("world", Json.mkObj
      [ ("own_physical", toJson c.ownPhysical)
      , ("model_own_physical", symbolJson start.requestId)
      , ("purpose", toJson start.purpose.toWire)
      , ("session", symbolJson start.sessionId)
      , ("principal", symbolJson start.principal)
      , ("queue_scope_agent", symbolJson start.queue.scope.agent)
      , ("queue_scope_session", symbolJson start.queue.scope.session)
      , ("queue_active", optionalSymbolJson start.queue.active)
      , ("lease_pending", toJson (start.lease.request == .pending))
      , ("lease_vacant", toJson (start.lease.lease == .vacant))
      , ("lease_now", toJson start.lease.now)
      , ("claimed_absent", toJson start.claimed.isNone)
      , ("terminal_selection_absent", toJson start.terminalSelection.isNone)
      , ("gate_actor", toJson (9 : Nat))
      , ("gate_phase", toJson "storage")
      , ("gate_independent", toJson start.gateSchedule.independent)
      , ("gate_sibling_waiting", toJson start.gateSchedule.siblingWaiting) ])
    , ("generation", toJson (7 : Nat))
    , ("duration", toJson (20 : Nat))
    , ("deadline", toJson (30 : Nat))
    , ("now", toJson (1 : Nat))
    , ("expected_activation", decision.map (fun value => bindingJson value.binding)
        |>.getD Json.null)
    , ("expected_claimed", toJson result.isSome)
    , ("expected_lease_active", toJson (match result with
        | some after => match after.lease.lease with
          | .active _ _ _ => true
          | _ => false
        | none => false))
    , ("expected_claimed_binding", (result.bind (·.claimed)).map (fun binding =>
        match binding.evidence with
        | .titleAudit title => bindingJson title
        | _ => Json.null) |>.getD Json.null)
    , ("expected_queue_active", optionalSymbolJson ((result.getD start).queue.active))
    , ("expected_queue_unchanged", toJson ((result.getD start).queue == start.queue))
    , ("expected_own_physical", symbolJson ((result.getD start).requestId)) ]

def casesJson : String := (toJson (cases.map caseJson)).compress

end Conformance.TitleAdmission
