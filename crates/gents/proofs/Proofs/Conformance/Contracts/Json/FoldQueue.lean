import Proofs.Session.SessionQueue.FoldCases
import Proofs.Session.SessionQueue.ManagementCases
import Proofs.Session.SessionQueue.InputEditCases
import Proofs.Runtime.CanonicalOutput.Execution.FoldPublicationCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.FoldQueueContracts

open SessionQueue SessionQueue.FoldCases Conformance.Contracts

def entryJson (entry : QueueEntry) : String :=
  "{\"request_id\":" ++ toString entry.requestId ++
  ",\"execution_origin\":" ++ jsonString entry.origin.toDefraDB ++
  ",\"delivery\":" ++ jsonString entry.delivery.toDefraDB ++
  ",\"order_key\":" ++ toString entry.orderKey ++
  ",\"source\":" ++ jsonString entry.source.toDefraDB ++
  ",\"policy\":" ++ jsonString entry.policy.toDefraDB ++
  ",\"queued_after\":" ++ jsonOptionalNat entry.queuedAfter ++
  ",\"requester_id\":" ++ jsonOptionalNat entry.requester ++
  ",\"turn_context\":" ++ toString entry.turnContext ++
  ",\"fresh\":" ++ jsonOptionalBool (some entry.fresh) ++ "}"

def eventJson : Event → String
  | .enqueue entry => "{\"kind\":\"enqueue\",\"entry\":" ++ entryJson entry ++ "}"
  | .claim admitted =>
      "{\"kind\":\"claim\",\"admitted\":" ++ jsonArray (admitted.map toString) ++ "}"
  | .consume => "{\"kind\":\"consume\"}"
  | .finish => "{\"kind\":\"finish\"}"

def authoredKeyJson : AuthoredKey → String
  | .context => "{\"kind\":\"context\"}"
  | .prompt => "{\"kind\":\"prompt\"}"
  | .folded id => "{\"kind\":\"folded\",\"request_id\":" ++ toString id ++ "}"

def turnInputCaseJson (entry : String × TurnInput String) : String :=
  let input := entry.2
  "{\"name\":" ++ jsonString entry.1 ++
  ",\"context\":" ++ (match input.context with | none => "null" | some c => jsonString c) ++
  ",\"head\":" ++ jsonString input.head ++
  ",\"folded\":" ++ jsonArray (input.folded.map fun (id, content) =>
    "{\"request_id\":" ++ toString id ++ ",\"content\":" ++ jsonString content ++ "}") ++
  ",\"authored\":" ++ jsonArray (input.authored.map fun (key, content) =>
    "{\"key\":" ++ authoredKeyJson key ++ ",\"content\":" ++ jsonString content ++ "}") ++
  ",\"provider_input\":" ++ jsonArray (input.providerInput.map jsonString) ++ "}"

def turnInputCasesJson : String := jsonArray (turnInputCases.map turnInputCaseJson)

def publicationStepJson : CanonicalOutput.Execution.FoldPublication.Step → String
  | .observeDeadline now deadline =>
      "{\"kind\":\"observe_deadline\",\"now\":" ++ toString now ++
      ",\"deadline\":" ++ toString deadline ++ "}"
  | .cancelFirstPending => "{\"kind\":\"cancel_first_pending\"}"
  | .enqueueSteering requestId =>
      "{\"kind\":\"enqueue_steering\",\"request_id\":" ++ toString requestId ++ "}"
  | .intake generation safe =>
      "{\"kind\":\"intake\",\"generation\":" ++ toString generation ++
      ",\"safe_boundary\":" ++ jsonOptionalBool (some safe) ++ "}"
  | .finishOrIntake generation safe =>
      "{\"kind\":\"finish_or_intake\",\"generation\":" ++ toString generation ++
      ",\"safe_boundary\":" ++ jsonOptionalBool (some safe) ++ "}"
  | .publishPrompt generation =>
      "{\"kind\":\"publish_prompt\",\"generation\":" ++ toString generation ++ "}"
  | .publishChangedPrompt generation =>
      "{\"kind\":\"publish_changed_prompt\",\"generation\":" ++ toString generation ++ "}"
  | .publishFolded generation requestId =>
      "{\"kind\":\"publish_folded\",\"generation\":" ++ toString generation ++
        ",\"request_id\":" ++ toString requestId ++ "}"
  | .acceptTurn generation =>
      "{\"kind\":\"accept_turn\",\"generation\":" ++ toString generation ++ "}"
  | .recover expected fresh =>
      "{\"kind\":\"recover\",\"expected\":" ++ toString expected ++
        ",\"fresh\":" ++ toString fresh ++ "}"
  | .terminalize generation =>
      "{\"kind\":\"terminalize\",\"generation\":" ++ toString generation ++ "}"
  | .finish => "{\"kind\":\"finish\"}"

def publicationObservationJson
    (value : CanonicalOutput.Execution.FoldPublication.Observation) : String :=
  "{\"accepted\":" ++ jsonOptionalBool (some value.accepted) ++
  ",\"active\":" ++ jsonOptionalNat value.active ++
  ",\"pending\":" ++ jsonArray (value.pending.map toString) ++
  ",\"folding\":" ++ jsonArray (value.folding.map toString) ++
  ",\"terminal\":" ++ jsonArray (value.terminal.map toString) ++
  ",\"authored_keys\":" ++ jsonArray (value.authoredKeys.map jsonString) ++ "}"

def publicationCasesJsonFor (cases : List (String × List CanonicalOutput.Execution.FoldPublication.Step)) : String :=
  jsonArray (cases.map fun (name, steps) =>
  "{\"name\":" ++ jsonString name ++
  ",\"head\":902,\"selected\":903" ++
  ",\"steps\":" ++ jsonArray (steps.map publicationStepJson) ++
  ",\"expected\":" ++ (match CanonicalOutput.Execution.FoldPublication.run steps with
    | none => "null"
    | some observations => jsonArray (observations.map publicationObservationJson)) ++ "}")

def publicationCasesJson : String :=
  publicationCasesJsonFor CanonicalOutput.Execution.FoldPublication.cases

def steeringPublicationCasesJson : String :=
  publicationCasesJsonFor CanonicalOutput.Execution.FoldPublication.steeringCases

def retrySelectionCasesJson : String := jsonArray (retrySelectionCases.map fun value =>
  "{\"name\":" ++ jsonString value.name ++
  ",\"parent_published\":" ++ jsonOptionalBool (some value.parentPublished) ++
  ",\"selected\":" ++ jsonArray (value.selected.map toString) ++
  ",\"resume\":" ++ jsonOptionalBool (some value.resume) ++
  ",\"answered\":" ++ jsonArray (value.answered.map toString) ++ "}")

def managementStateJson (state : SessionQueueState) : String :=
  "{\"active\":" ++ jsonOptionalNat state.active ++
  ",\"pending\":" ++ jsonArray (state.pending.map entryJson) ++
  ",\"folding\":" ++ jsonArray (state.folding.map entryJson) ++
  ",\"terminal\":" ++ jsonArray ((state.terminal.sort (· ≤ ·)).map toString) ++ "}"

def managementOperationJson : SessionQueue.ManagementCases.Operation → String
  | .intake active admitted safe =>
      "{\"kind\":\"intake\",\"active\":" ++ entryJson active ++
      ",\"admitted\":" ++ jsonArray (admitted.map toString) ++
      ",\"safe_boundary\":" ++ jsonOptionalBool (some safe) ++ "}"
  | .replace caller expected offset count replacements =>
      "{\"kind\":\"replace\",\"caller\":" ++ jsonOptionalNat caller ++
      ",\"expected\":" ++ jsonArray (expected.map toString) ++
      ",\"offset\":" ++ toString offset ++ ",\"count\":" ++ toString count ++
      ",\"replacements\":" ++ jsonArray (replacements.map entryJson) ++ "}"

def managementCasesJson : String :=
  jsonArray (SessionQueue.ManagementCases.cases.map fun value =>
    "{\"name\":" ++ jsonString value.name ++
    ",\"before\":" ++ managementStateJson value.before ++
    ",\"operation\":" ++ managementOperationJson value.operation ++
    ",\"expected\":" ++ (match value.after with
      | none => "null"
      | some state => managementStateJson state) ++ "}")

def editReceiptJson (r : SessionQueue.InputEdit.Receipt) : String :=
  "{\"command_id\":" ++ toString r.commandId ++
  ",\"digest\":" ++ toString r.digest ++ ",\"outcome\":" ++
  jsonString (match r.outcome with | .applied => "applied" | .rejected => "rejected") ++
  ",\"request_ids\":" ++ jsonArray (r.requestIds.map toString) ++ "}"

def editStateJson (s : SessionQueue.InputEdit.State) : String :=
  "{\"queue\":" ++ managementStateJson s.queue ++
  ",\"receipts\":" ++ jsonArray (s.receipts.map editReceiptJson) ++ "}"

def editCommandJson (command : SessionQueue.InputEdit.Command) : String :=
  "{\"id\":" ++ toString command.id ++
  ",\"digest\":" ++ toString command.digest ++ ",\"caller\":" ++ jsonOptionalNat command.caller ++
  ",\"issued_at\":" ++ toString command.issuedAt ++ ",\"expires_at\":" ++ toString command.expiresAt ++
  ",\"expected\":" ++ jsonArray (command.expected.map toString) ++
  ",\"offset\":" ++ toString command.offset ++ ",\"count\":" ++ toString command.count ++
  ",\"replacements\":" ++ jsonArray (command.replacements.map entryJson) ++ "}"

def editAuthorityJson (a : SessionQueue.InputEdit.Authority) : String :=
  "{\"signature_valid\":" ++ jsonOptionalBool (some a.signatureValid) ++
  ",\"replacements_valid\":" ++ jsonOptionalBool (some a.replacementsValid) ++
  ",\"session_owned\":" ++ jsonOptionalBool (some a.sessionOwned) ++
  ",\"editor\":" ++ jsonString (match a.editor with
    | .requester => "requester" | .workingAgent => "working_agent"
    | .admin => "admin" | .other => "other") ++
  ",\"command_identity_unique\":" ++ jsonOptionalBool (some a.commandIdentityUnique) ++ "}"

def editStepJson : SessionQueue.InputEditCases.Step → String
  | .apply now a command =>
      "{\"kind\":\"apply\",\"now\":" ++ toString now ++
      ",\"authority\":" ++ editAuthorityJson a ++ ",\"command\":" ++ editCommandJson command ++ "}"
  | .loseReceipts => "{\"kind\":\"lose_receipts\"}"
  | .otherSession session =>
      "{\"kind\":\"other_session\",\"session\":" ++ toString session ++ "}"

def inputEditCasesJson : String :=
  jsonArray (SessionQueue.InputEditCases.cases.map fun c =>
    let result := SessionQueue.InputEditCases.run c
    "{\"name\":" ++ jsonString c.name ++
    ",\"initial\":" ++ editStateJson SessionQueue.InputEditCases.state ++
    ",\"history\":" ++ jsonArray (c.history.map editStepJson) ++
    ",\"before\":" ++ editStateJson c.before ++
    ",\"now\":" ++ toString c.now ++ ",\"command\":" ++ editCommandJson c.command ++
    ",\"authority\":" ++ editAuthorityJson c.authority ++
    ",\"after\":" ++ editStateJson result.1 ++ ",\"receipt\":" ++
    (match result.2 with | none => "null" | some r => editReceiptJson r) ++ "}")

def inputEditClientObservationCasesJson : String :=
  jsonArray (SessionQueue.InputEditCases.clientObservationCases.map fun c =>
    let observation := SessionQueue.InputEditCases.runClientObservation c
    "{\"name\":" ++ jsonString c.name ++
    ",\"receipt_outcome\":" ++ jsonString (match c.receiptOutcome with
      | .applied => "applied" | .rejected => "rejected") ++
    ",\"projection_refresh_succeeded\":" ++ jsonOptionalBool (some c.projectionRefreshSucceeded) ++
    ",\"command_outcome\":" ++ jsonString (match observation.commandOutcome with
      | .applied => "applied" | .rejected => "rejected") ++
    ",\"projection_fresh\":" ++ jsonOptionalBool (some observation.projectionFresh) ++ "}")

section Families

open Lean

deriving instance ToJson for QueueDelivery, QueueSource, QueuePolicy, ExecutionOrigin

structure GoalAssignmentWire where
  goal : String
  opening : String
  deriving ToJson

structure EntryWire where
  requestId : Nat
  createdAt : Nat
  orderKey : Nat
  executionOrigin : ExecutionOrigin
  delivery : QueueDelivery
  source : QueueSource
  policy : QueuePolicy
  queueKey : Option Nat
  queuedAfter : Option Nat
  requesterId : Option Nat
  turnContext : Nat
  executionDirectory : Option Nat
  goalAssignment : Option GoalAssignmentWire
  fresh : Bool
  deriving ToJson

def EntryWire.ofModel (entry : QueueEntry) : EntryWire :=
  { requestId := entry.requestId, createdAt := entry.createdAt, orderKey := entry.orderKey
    executionOrigin := entry.origin, delivery := entry.delivery, source := entry.source
    policy := entry.policy, queueKey := entry.queueKey, queuedAfter := entry.queuedAfter
    requesterId := entry.requester, turnContext := entry.turnContext
    executionDirectory := entry.executionDirectory
    goalAssignment := entry.goalAssignment.map fun (goal, opening) => { goal, opening }
    fresh := entry.fresh }

def EntryWire.toModel (entry : EntryWire) : QueueEntry :=
  { requestId := entry.requestId, createdAt := entry.createdAt, orderKey := entry.orderKey
    origin := entry.executionOrigin, delivery := entry.delivery, source := entry.source
    policy := entry.policy, queueKey := entry.queueKey, queuedAfter := entry.queuedAfter
    requester := entry.requesterId, turnContext := entry.turnContext
    executionDirectory := entry.executionDirectory
    goalAssignment := entry.goalAssignment.map fun g => (g.goal, g.opening)
    fresh := entry.fresh }

inductive EventKind where
  | enqueue | claim | consume | finish
  deriving ToJson

structure EventWire where
  kind : EventKind
  entry : Option EntryWire
  admitted : List Nat
  deriving ToJson

def EventWire.ofModel : Event → EventWire
  | .enqueue entry => { kind := .enqueue, entry := some (EntryWire.ofModel entry), admitted := [] }
  | .claim admitted => { kind := .claim, entry := none, admitted }
  | .consume => { kind := .consume, entry := none, admitted := [] }
  | .finish => { kind := .finish, entry := none, admitted := [] }

def EventWire.toModel (event : EventWire) : Option Event :=
  match event.kind with
  | .enqueue => event.entry.map fun entry => .enqueue entry.toModel
  | .claim => some (.claim event.admitted)
  | .consume => some .consume
  | .finish => some .finish

structure FoldInput where
  name : String
  nodeId : Nat
  sessionId : Nat
  inputs : List EventWire
  deriving ToJson

structure ClaimWire where
  head : Nat
  folded : List Nat
  deriving ToJson

structure FoldObservation where
  active : Option Nat
  pending : List Nat
  folding : List Nat
  terminal : List Nat
  claims : List ClaimWire
  deriving ToJson

@[conformance_family]
def foldScript : Family FoldInput (Option FoldObservation) where
  inputs := cases.map fun (name, events) =>
    { name, nodeId := queue.scope.node, sessionId := queue.scope.session
      inputs := events.map EventWire.ofModel }
  model := fun i => do
    let events ← i.inputs.mapM EventWire.toModel
    let observed ← observation events
    pure { active := observed.active, pending := observed.pending, folding := observed.folding
           terminal := observed.terminal
           claims := observed.claims.map fun (head, folded) => { head, folded } }

structure HandoverInput where
  name : String
  pending : List EntryWire
  admitted : List Nat
  deriving ToJson

structure HandoverQueue where
  active : Option Nat
  folding : List Nat
  pending : List Nat
  deriving ToJson

open CanonicalOutput.Execution.Handover.Cases in
@[conformance_family]
def handoverClaim : Family HandoverInput (Option HandoverQueue) where
  inputs := foldClaimCases.map fun value =>
    { name := value.name, pending := value.pending.map EntryWire.ofModel, admitted := value.admitted }
  model := fun i => do
    let claimed ← foldClaim
      { name := i.name, pending := i.pending.map EntryWire.toModel, admitted := i.admitted }
    pure { active := claimed.active, folding := claimed.folding.map (·.requestId)
           pending := claimed.pending.map (·.requestId) }

end Families

end Conformance.FoldQueueContracts
