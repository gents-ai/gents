import Proofs.Session.FoldCases
import Proofs.Session.ManagementCases
import Proofs.Session.InputEditCases
import Proofs.CanonicalOutput.Execution.FoldPublicationCases
import Proofs.Conformance.Contracts.Json.Helpers

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
  jsonString (match r.outcome with | .applied => "applied" | .rejected => "rejected") ++ "}"

def editStateJson (s : SessionQueue.InputEdit.State) : String :=
  "{\"queue\":" ++ managementStateJson s.queue ++
  ",\"receipts\":" ++ jsonArray (s.receipts.map editReceiptJson) ++ "}"

def inputEditCasesJson : String :=
  jsonArray (SessionQueue.InputEditCases.cases.map fun c =>
    let result := SessionQueue.InputEditCases.run c
    let a := c.authority
    let command := c.command
    "{\"name\":" ++ jsonString c.name ++ ",\"before\":" ++ editStateJson c.before ++
    ",\"now\":" ++ toString c.now ++ ",\"command\":{\"id\":" ++ toString command.id ++
    ",\"digest\":" ++ toString command.digest ++ ",\"caller\":" ++ jsonOptionalNat command.caller ++
    ",\"has_peer\":" ++ jsonOptionalBool (some command.hasPeer) ++
    ",\"issued_at\":" ++ toString command.issuedAt ++ ",\"expires_at\":" ++ toString command.expiresAt ++
    ",\"expected\":" ++ jsonArray (command.expected.map toString) ++
    ",\"offset\":" ++ toString command.offset ++ ",\"count\":" ++ toString command.count ++
    ",\"replacements\":" ++ jsonArray (command.replacements.map entryJson) ++ "}" ++
    ",\"authority\":{\"signature_valid\":" ++ jsonOptionalBool (some a.signatureValid) ++
    ",\"replacements_valid\":" ++ jsonOptionalBool (some a.replacementsValid) ++
    ",\"session_owned\":" ++ jsonOptionalBool (some a.sessionOwned) ++
    ",\"local_self\":" ++ jsonOptionalBool (some a.localSelf) ++
    ",\"requester_is_node\":" ++ jsonOptionalBool (some a.requesterIsNode) ++
    ",\"enrollment_fresh\":" ++ jsonOptionalBool (some a.enrollmentFresh) ++
    ",\"route_applied\":" ++ jsonOptionalBool (some a.routeApplied) ++
    ",\"command_identity_unique\":" ++ jsonOptionalBool (some a.commandIdentityUnique) ++ "}" ++
    ",\"after\":" ++ editStateJson result.1 ++ ",\"receipt\":" ++
    (match result.2 with | none => "null" | some r => editReceiptJson r) ++ "}")

end Conformance.FoldQueueContracts
