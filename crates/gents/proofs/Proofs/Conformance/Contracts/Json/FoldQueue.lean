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

def caseJson (entry : String × List Event) : String :=
  "{\"name\":" ++ jsonString entry.1 ++
  ",\"node_id\":" ++ toString queue.scope.node ++
  ",\"session_id\":" ++ toString queue.scope.session ++
  ",\"inputs\":" ++ jsonArray (entry.2.map eventJson) ++
  ",\"expected\":" ++ (match observation entry.2 with
    | none => "null"
    | some observed =>
        "{\"active\":" ++ jsonOptionalNat observed.active ++
        ",\"pending\":" ++ jsonArray (observed.pending.map toString) ++
        ",\"folding\":" ++ jsonArray (observed.folding.map toString) ++
        ",\"terminal\":" ++ jsonArray (observed.terminal.map toString) ++
        ",\"claims\":" ++ jsonArray (observed.claims.map fun (head, folded) =>
          "{\"head\":" ++ toString head ++
          ",\"folded\":" ++ jsonArray (folded.map toString) ++ "}") ++ "}") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

inductive AuthoredKeyKind where
  | context
  | prompt
  | folded
  deriving Lean.ToJson

structure AuthoredKeyWire where
  kind : AuthoredKeyKind
  requestId : Option Nat
  deriving Lean.ToJson

def AuthoredKeyWire.ofModel : AuthoredKey → AuthoredKeyWire
  | .context => { kind := .context, requestId := none }
  | .prompt => { kind := .prompt, requestId := none }
  | .folded id => { kind := .folded, requestId := some id }

structure FoldedWire where
  requestId : Nat
  content : String
  deriving Lean.ToJson

structure AuthoredWire where
  key : AuthoredKeyWire
  content : String
  deriving Lean.ToJson

structure TurnInputCase where
  name : String
  context : Option String
  head : String
  folded : List FoldedWire
  deriving Lean.ToJson

structure TurnInputObservation where
  authored : List AuthoredWire
  providerInput : List String
  deriving Lean.ToJson

@[conformance_family]
def turnInput : Family TurnInputCase TurnInputObservation where
  inputs := turnInputCases.map fun (name, input) =>
    { name, context := input.context, head := input.head
      folded := input.folded.map fun (requestId, content) => { requestId, content } }
  model := fun i =>
    let input : TurnInput String :=
      ⟨i.context, i.head, i.folded.map fun folded => (folded.requestId, folded.content)⟩
    { authored := input.authored.map fun (key, content) =>
        { key := AuthoredKeyWire.ofModel key, content }
      providerInput := input.providerInput }

open CanonicalOutput.Execution.Handover.Cases in
def handoverCaseJson (value : FoldClaimCase) : String :=
  "{\"name\":" ++ jsonString value.name ++
  ",\"pending\":" ++ jsonArray (value.pending.map entryJson) ++
  ",\"admitted\":" ++ jsonArray (value.admitted.map toString) ++
  ",\"expected\":" ++ (match foldClaim value with
    | none => "null"
    | some queue =>
        "{\"active\":" ++ jsonOptionalNat queue.active ++
        ",\"folding\":" ++ jsonArray (queue.folding.map (toString ·.requestId)) ++
        ",\"pending\":" ++ jsonArray (queue.pending.map (toString ·.requestId)) ++ "}") ++ "}"

def handoverCasesJson : String :=
  jsonArray (CanonicalOutput.Execution.Handover.Cases.foldClaimCases.map handoverCaseJson)

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

structure RetrySelectionInput where
  name : String
  parentPublished : Bool
  selected : List Nat
  deriving Lean.ToJson

structure RetrySelectionOutput where
  resume : Bool
  answered : List Nat
  deriving Lean.ToJson

@[conformance_family]
def retrySelection : Family RetrySelectionInput RetrySelectionOutput where
  inputs := retrySelectionCases.map fun c =>
    { name := c.name, parentPublished := c.parentPublished, selected := c.selected }
  model := fun i =>
    let value : RetrySelectionCase := ⟨i.name, i.parentPublished, i.selected⟩
    { resume := value.resume, answered := value.answered }

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

end Conformance.FoldQueueContracts
