import Proofs.CanonicalOutput.Execution.WorkerCapacity
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.RequestExecutionLease

/-! Executable request-worker operations. Expectations are evaluations of the
same owner functions used in the scheduler proof, not copied fixture results. -/
namespace Conformance.WorkerCapacityContracts

open CanonicalOutput.Execution
open CanonicalOutput.Execution.WorkerCapacity
open CanonicalOutput
open Conformance.Contracts

inductive Operation where
  | admitFresh (ticket : Ticket)
  | wait (generation : CanonicalOutput.Execution.Generation) (document : DocId)
  | resume (generation : CanonicalOutput.Execution.Generation) (cancellationAllows : Bool)
  | waitExisting (generation : CanonicalOutput.Execution.Generation)
      (document : DocId) (selection : ExistingChildSelection)
  | resumeExisting (generation : CanonicalOutput.Execution.Generation)
      (selection : ExistingChildSelection) (cancellationAllows : Bool)
  | release (ticket : Ticket)

structure Case where
  name : String
  worldFixture : String
  world : World
  pre : State
  operation : Operation

def Case.evaluate (c : Case) : Option State :=
  match c.operation with
  | .admitFresh ticket => WorkerCapacity.admitFresh c.pre ticket
  | .wait generation document => waitForChild c.pre c.world generation document
  | .resume generation allows => resumeAfterChild c.pre c.world generation allows
  | .waitExisting generation document selection =>
      waitForExistingChild c.pre c.world generation selection document
  | .resumeExisting generation selection allows =>
      resumeAfterExistingChild c.pre c.world generation selection allows
  | .release ticket => some (WorkerCapacity.release c.pre ticket)

private opaque baseCasesOption : Option (List Case) := do
  let running ← runningChild
  let completed ← completedChild
  let empty := initial 1 1
  let parent ← admitFresh empty (10, 7)
  let waiting ← waitForChild parent running 7 600
  let child ← admitFresh waiting (42, 9)
  let freed := release child (42, 9)
  let noParking ← admitFresh (initial 1 0) (10, 7)
  let stale := { completed with lease := { completed.lease with now := 11 } }
  let existingRunning ← existingWaitRunning
  let existingCompleted ← existingWaitCompleted
  let existingParent ← admitFresh empty (11, 8)
  let existingParked ← waitForExistingChild existingParent existingRunning 8
    existingChildSelection 600
  let existingChild ← admitFresh existingParked (42, 9)
  let existingFreed := release existingChild (42, 9)
  let terminalSelection := { existingChildSelection with edge :=
    { existingChildSelection.edge with lifecycle := .completed } }
  let unauthorizedSelection := { existingChildSelection with
    callerOwner := ⟨"session-1", "did:other", none⟩ }
  let wrongChildSelection := { existingChildSelection with edge :=
    { existingChildSelection.edge with childRequestId := 43 } }
  let wrongControlSelection := { existingChildSelection with controlDocument := 600 }
  let staleExistingRunning := { existingRunning with lease :=
    { existingRunning.lease with lease := .active 9 5 10 } }
  let staleExistingCompleted := { existingCompleted with lease :=
    { existingCompleted.lease with lease := .active 9 5 10 } }
  pure
    [ ⟨"parent_admit", "running", running, empty, .admitFresh (10, 7)⟩
    , ⟨"child_blocked_before_yield", "running", running, parent, .admitFresh (42, 9)⟩
    , ⟨"parent_wait_for_accepted_child", "running", running, parent, .wait 7 600⟩
    , ⟨"child_admit_after_yield", "running", running, waiting, .admitFresh (42, 9)⟩
    , ⟨"parked_parent_cannot_fresh_admit", "running", running, waiting, .admitFresh (10, 7)⟩
    , ⟨"parked_overflow_refused", "running", running, noParking, .wait 7 600⟩
    , ⟨"child_releases_worker", "running", running, child, .release (42, 9)⟩
    , ⟨"parent_resume_after_child", "completed", completed, freed, .resume 7 true⟩
    , ⟨"unfinished_child_refused", "running", running, freed, .resume 7 true⟩
    , ⟨"cancelled_parent_refused", "completed", completed, freed, .resume 7 false⟩
    , ⟨"stale_generation_refused", "completed", completed, freed, .resume 8 true⟩
    , ⟨"expired_lease_refused", "expired", stale, freed, .resume 7 true⟩
    , ⟨"wrong_dependency_refused", "completed", completed,
        { freed with dependencies := [((10, 7), 601)] }, .resume 7 true⟩
    , ⟨"orphan_dependency_cannot_fresh_admit", "running", running,
        { empty with dependencies := [((10, 7), 600)] }, .admitFresh (10, 7)⟩
    , ⟨"existing_parent_admit", "existing_running", existingRunning,
        empty, .admitFresh (11, 8)⟩
    , ⟨"existing_wait_prior_request_bridge", "existing_running", existingRunning,
        existingParent, .waitExisting 8 600 existingChildSelection⟩
    , ⟨"existing_child_admit_after_yield", "existing_running", existingRunning,
        existingParked, .admitFresh (42, 9)⟩
    , ⟨"existing_child_releases_worker", "existing_running", existingRunning,
        existingChild, .release (42, 9)⟩
    , ⟨"existing_parent_resume", "existing_completed", existingCompleted,
        existingFreed, .resumeExisting 8 terminalSelection true⟩
    , ⟨"existing_cancelled_current_caller_refused", "existing_completed", existingCompleted,
        existingFreed, .resumeExisting 8 terminalSelection false⟩
    , ⟨"existing_substituted_bridge_document_refused", "existing_running", existingRunning,
        existingParent, .waitExisting 8 601 existingChildSelection⟩
    , ⟨"existing_unauthorized_owner_refused", "existing_running", existingRunning,
        existingParent, .waitExisting 8 600 unauthorizedSelection⟩
    , ⟨"existing_stale_current_wait_refused", "existing_stale_running", staleExistingRunning,
        existingParent, .waitExisting 8 600 existingChildSelection⟩
    , ⟨"existing_unfinished_bridge_refused", "existing_running", existingRunning,
        existingFreed, .resumeExisting 8 existingChildSelection true⟩
    , ⟨"existing_stale_current_resume_refused", "existing_stale_completed", staleExistingCompleted,
        existingFreed, .resumeExisting 8 terminalSelection true⟩
    , ⟨"existing_wrong_child_edge_refused", "existing_running", existingRunning,
        existingParent, .waitExisting 8 600 wrongChildSelection⟩
    , ⟨"existing_wrong_control_tool_refused", "existing_running", existingRunning,
        existingParent, .waitExisting 8 600 wrongControlSelection⟩
    ]

private opaque handoffCasesOption : Option (List Case) := do
  let running ← runningChild
  let modeOnly ← backgroundedChild
  let receipted ← receiptedBackgroundChild
  let parent ← admitFresh (initial 1 1) (10, 7)
  let parked ← waitForChild parent running 7 600
  let child ← admitFresh parked (42, 9)
  let freed := release child (42, 9)
  let existingRunning ← existingWaitRunning
  let existingModeOnly ← existingWaitModeOnlyRunning
  let existingReceipted ← existingWaitHandoffRunning
  let existingParent ← admitFresh (initial 1 1) (11, 8)
  let existingParked ← waitForExistingChild existingParent existingRunning 8
    existingChildSelection 600
  let existingChild ← admitFresh existingParked (42, 9)
  let existingFreed := release existingChild (42, 9)
  pure
    [ ⟨"background_mode_without_receipt_refused", "background_mode_only", modeOnly,
        freed, .resume 7 true⟩
    , ⟨"background_handoff_receipt_resumes", "background_receipted", receipted,
        freed, .resume 7 true⟩
    , ⟨"existing_background_mode_without_receipt_refused", "existing_mode_only",
        existingModeOnly, existingFreed, .resumeExisting 8 existingChildSelection true⟩
    , ⟨"existing_background_handoff_receipt_resumes", "existing_receipted",
        existingReceipted, existingFreed, .resumeExisting 8 existingChildSelection true⟩
    ]

def casesOption : Option (List Case) := do
  let base ← baseCasesOption
  let handoffs ← handoffCasesOption
  pure (base ++ handoffs)

private def ticketJson (ticket : Ticket) : String :=
  "[" ++ toString ticket.1 ++ "," ++ toString ticket.2 ++ "]"

private def fixtureTickets : List Ticket := [(10, 7), (11, 8), (42, 9)]

private def stateJson (state : State) : String :=
  "{" ++ "\"active_limit\":" ++ toString state.activeLimit ++ ","
    ++ "\"parked_limit\":" ++ toString state.parkedLimit ++ ","
    ++ "\"active\":" ++ jsonArray ((fixtureTickets.filter (· ∈ state.active)).map ticketJson) ++ ","
    ++ "\"parked\":" ++ jsonArray ((fixtureTickets.filter (· ∈ state.parked)).map ticketJson) ++ ","
    ++ "\"dependencies\":" ++ jsonArray (state.dependencies.map fun (ticket, document) =>
      "{" ++ "\"ticket\":" ++ ticketJson ticket ++ ","
        ++ "\"document\":" ++ toString document ++ "}") ++ "}"

private def optionalStateJson : Option State → String
  | none => "null"
  | some state => stateJson state

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def ownerJson (owner : DescendantGraph.SessionOwner) : String :=
  "{" ++ "\"session_id\":" ++ jsonString owner.sessionId ++ ","
    ++ "\"agent_did\":" ++ jsonString owner.agentDid ++ ","
    ++ "\"requester_did\":" ++ (owner.requesterDid.map jsonString).getD "null" ++ "}"

private def edgeJson (edge : DescendantGraph.Edge) : String :=
  let awaitMode := match edge.awaitMode with
    | .foreground => "foreground" | .background => "background"
  let materialization := match edge.materialization with
    | .pending => "pending" | .local => "local" | .replicated => "replicated"
  let lifecycle := match edge.lifecycle with
    | .pending => "pending" | .running => "running" | .completed => "completed"
    | .failed => "failed" | .cancelled => "cancelled"
  "{" ++ "\"root_request_id\":" ++ toString edge.rootRequestId ++ ","
    ++ "\"root_session_id\":" ++ toString edge.rootSessionId ++ ","
    ++ "\"parent_request_id\":" ++ toString edge.parentRequestId ++ ","
    ++ "\"parent_tool_call_id\":" ++ toString edge.parentToolCallId ++ ","
    ++ "\"child_request_id\":" ++ toString edge.childRequestId ++ ","
    ++ "\"child_session_id\":" ++ jsonOptionalNat edge.childSessionId ++ ","
    ++ "\"owner_principal\":" ++ toString edge.ownerPrincipal ++ ","
    ++ "\"control_principal\":" ++ toString edge.controlPrincipal ++ ","
    ++ "\"child_principal\":" ++ toString edge.childPrincipal ++ ","
    ++ "\"behavior_id\":" ++ toString edge.behaviorId ++ ","
    ++ "\"lineage_id\":" ++ toString edge.lineageId ++ ","
    ++ "\"await_mode\":" ++ jsonString awaitMode ++ ","
    ++ "\"materialization\":" ++ jsonString materialization ++ ","
    ++ "\"lifecycle\":" ++ jsonString lifecycle ++ ","
    ++ "\"bridge_durable\":" ++ boolJson edge.bridgeDurable ++ ","
    ++ "\"physical_corroborated\":" ++ boolJson edge.physicalCorroborated ++ ","
    ++ "\"direct_from_root\":" ++ boolJson edge.directFromRoot ++ "}"

private def selectionJson (selection : ExistingChildSelection) : String :=
  "{" ++ "\"viewer\":{" ++
      "\"root_request_id\":" ++ toString selection.viewer.rootRequestId ++ ","
      ++ "\"root_principal\":" ++ toString selection.viewer.rootPrincipal ++ ","
      ++ "\"root_session_id\":" ++ toString selection.viewer.rootSessionId ++ ","
      ++ "\"lineage_id\":" ++ toString selection.viewer.lineageId ++ "},"
    ++ "\"caller_owner\":" ++ ownerJson selection.callerOwner ++ ","
    ++ "\"bridge_owner\":" ++ ownerJson selection.bridgeOwner ++ ","
    ++ "\"edge\":" ++ edgeJson selection.edge ++ ","
    ++ "\"bridge_document\":" ++ toString selection.bridgeDocument ++ ","
    ++ "\"control_document\":" ++ toString selection.controlDocument ++ "}"

private def operationJson : Operation → String
  | .admitFresh ticket =>
      "{\"kind\":\"admit_fresh\",\"ticket\":" ++ ticketJson ticket ++ "}"
  | .wait generation document =>
      "{\"kind\":\"wait_for_child\",\"generation\":" ++ toString generation ++
        ",\"document\":" ++ toString document ++ "}"
  | .resume generation allows =>
      "{\"kind\":\"resume_after_child\",\"generation\":" ++ toString generation ++
        ",\"cancellation_allows\":" ++ (if allows then "true" else "false") ++ "}"
  | .waitExisting generation document selection =>
      "{\"kind\":\"wait_for_existing_child\",\"generation\":" ++
        toString generation ++ ",\"document\":" ++ toString document ++
        ",\"selection\":" ++ selectionJson selection ++ "}"
  | .resumeExisting generation selection allows =>
      "{\"kind\":\"resume_after_existing_child\",\"generation\":" ++
        toString generation ++ ",\"selection\":" ++ selectionJson selection ++
        ",\"cancellation_allows\":" ++ boolJson allows ++ "}"
  | .release ticket =>
      "{\"kind\":\"release\",\"ticket\":" ++ ticketJson ticket ++ "}"

private def operationGeneration : Operation → Nat
  | .admitFresh ticket | .release ticket => ticket.2
  | .wait generation _ | .resume generation _
  | .waitExisting generation _ _ | .resumeExisting generation _ _ => generation

private def selectedDocument (c : Case) : Option DocId :=
  match c.operation with
  | .wait _ document => some document
  | .resume generation _ => c.pre.dependencies.lookup (c.world.requestId, generation)
  | .waitExisting _ document _ => some document
  | .resumeExisting _ selection _ => some selection.bridgeDocument
  | _ => none

private def selectedToolJson (c : Case) : String :=
  match (selectedDocument c).bind (ownedToolByDocument? c.world) with
  | none => "null"
  | some tool =>
      "{" ++ "\"document\":" ++ toString tool.document ++ ","
        ++ "\"request_doc\":" ++ toString tool.requestDoc ++ ","
        ++ "\"session\":" ++ toString tool.session ++ ","
        ++ "\"accepted_sequence\":" ++ toString tool.acceptedSequence ++ ","
        ++ "\"state\":" ++ jsonString tool.context.state.toDefraDB ++ ","
        ++ "\"await_mode\":" ++ jsonString
          (match tool.context.awaitMode with
            | .foreground => "foreground" | .background => "background") ++ ","
        ++ "\"canonical_tool_delivered\":" ++
          boolJson (canonicalToolDelivered c.world tool) ++ ","
        ++ "\"child_request_id\":" ++ jsonOptionalNat tool.context.childRequestId ++ ","
        ++ "\"accepted_header_binds_generation\":" ++
          (if acceptedHeaderBindsToolGeneration c.world tool
              (operationGeneration c.operation) then "true" else "false") ++ ","
        ++ "\"accepted_header_binds_tool\":" ++
          boolJson (acceptedHeaderBindsTool c.world tool) ++ "}"

private def controlToolJson (c : Case) : String :=
  let selection := match c.operation with
    | .waitExisting _ _ selection | .resumeExisting _ selection _ => some selection
    | _ => none
  match selection.bind (fun value =>
      (ownedToolByDocument? c.world value.controlDocument).map (fun tool => (value, tool))) with
  | none => "null"
  | some (_, tool) =>
      "{" ++ "\"document\":" ++ toString tool.document ++ ","
        ++ "\"state\":" ++ jsonString tool.context.state.toDefraDB ++ ","
        ++ "\"current_wait_control\":" ++
          boolJson (currentWaitControl c.world (operationGeneration c.operation) tool) ++ "}"

/-- Exact owner inputs consumed by wait/resume. The accepted header is emitted
from the modeled world, and its generation binding is observed through the
existing projection owner. -/
private def worldInputJson (c : Case) : String :=
  let claim := match c.world.claimed with
    | none => "null"
    | some binding =>
        "{" ++ "\"physical_request\":" ++ toString binding.physicalRequest ++ ","
          ++ "\"logical_request\":" ++ toString binding.logicalRequest ++ ","
          ++ "\"session\":" ++ toString binding.session ++ "}"
  "{" ++ "\"request_id\":" ++ toString c.world.requestId ++ ","
    ++ "\"session_id\":" ++ toString c.world.sessionId ++ ","
    ++ "\"lease\":" ++ Conformance.RequestExecutionLeaseContracts.worldJson c.world.lease ++ ","
    ++ "\"claim\":" ++ claim ++ ","
    ++ "\"queue_active\":" ++ jsonOptionalNat c.world.queue.active ++ ","
    ++ "\"retry_request\":" ++ toString c.world.retry.request ++ ","
    ++ "\"current_claim\":" ++
      (if SessionComposition.currentClaim c.world then "true" else "false") ++ ","
    ++ "\"accepted_messages\":" ++ jsonArray (c.world.messages.map canonicalMessageJson) ++ ","
    ++ "\"selected_tool\":" ++ selectedToolJson c ++ ","
    ++ "\"control_tool\":" ++ controlToolJson c ++ "}"

def caseJson (c : Case) : String :=
  "{" ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"world_fixture\":" ++ jsonString c.worldFixture ++ ","
    ++ "\"world\":" ++ worldInputJson c ++ ","
    ++ "\"pre\":" ++ stateJson c.pre ++ ","
    ++ "\"operation\":" ++ operationJson c.operation ++ ","
    ++ "\"expected\":" ++ optionalStateJson c.evaluate ++ "}"

def casesJson : String := jsonArray ((casesOption.getD []).map caseJson)

theorem cases_reachable : casesOption.isSome = true := by native_decide

private def represented (state : State) : Bool :=
  decide (state.active ⊆ fixtureTickets.toFinset) &&
    decide (state.parked ⊆ fixtureTickets.toFinset)

theorem serialized_fixture_tickets_cover_all_states :
    (casesOption.getD []).all (fun c =>
      represented c.pre && c.evaluate.all represented) = true := by native_decide

theorem cases_have_expected_success_and_refusal :
    ((casesOption.getD []).filter (fun c => c.evaluate.isSome)).length = 12 ∧
      ((casesOption.getD []).filter (fun c => c.evaluate.isNone)).length = 19 := by
  native_decide

end Conformance.WorkerCapacityContracts
