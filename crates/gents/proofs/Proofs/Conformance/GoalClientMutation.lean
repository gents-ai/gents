import Proofs.GoalAutomation.ClientMutation
import Proofs.Conformance.GoalClientControl

namespace Conformance.GoalClientMutationContracts

open GoalAutomation.ClientMutation Conformance.Contracts

def initial : State := ⟨GoalClientControlContracts.initial .active, []⟩
def authority : AgentSession.CommandAuthority.Facts := ⟨true, true, false, true, true⟩
def facts : Facts := ⟨authority, true, true, none, "session-document"⟩
def pause : Command := ⟨10, 20, true, "session-document", 1, 10, initial.goal.revision, .pause⟩
def resume : Command :=
  { pause with
    id := 11
    digest := 21
    operation := .resume
    expected := { initial.goal.revision with composite := "revision-paused" } }

inductive Step where
  | apply (command : Command) (facts : Facts) (now : Nat) (next : String) (commit : Bool)
  | loseReceipts
  deriving DecidableEq, Repr

def Step.run (s : State) : Step → State
  | .apply command f now next commit => (GoalAutomation.ClientMutation.apply s command f now next commit).1
  | .loseReceipts => { s with receipts := [] }

structure Case where
  name : String
  history : List Step
  command : Command
  facts : Facts
  now : Nat
  next : String := "revision-next"
  commit : Bool := true
  deriving DecidableEq, Repr

def Case.before (c : Case) : State := c.history.foldl Step.run initial
def Case.after (c : Case) : State × Option Receipt :=
  GoalAutomation.ClientMutation.apply c.before c.command c.facts c.now c.next c.commit

def entry (name : String) (history : List Step) (command : Command) (f : Facts) (now : Nat) : Case :=
  { name, history, command, facts := f, now }

instance : Inhabited Case := ⟨entry "" [] pause facts 2⟩

def paused : List Step := [.apply pause facts 2 "revision-paused" true]
def resumed : List Step := paused ++ [.apply resume facts 3 "revision-resumed" true]

def cases : List Case :=
  [ entry "fresh_pause" [] pause facts 2
  , entry "invalid_signature" [] pause { facts with authority.signatureValid := false } 2
  , entry "unowned_session" [] pause { facts with authority.sessionOwned := false } 2
  , entry "revoked_enrollment" [] pause { facts with authority.enrollmentFresh := false } 2
  , entry "missing_route" [] pause { facts with authority.routeApplied := false } 2
  , entry "local_node_author" [] { pause with hasPeer := false }
      { facts with authority.requesterIsNode := true } 2
  , entry "local_foreign_author" [] { pause with hasPeer := false } facts 2
  , entry "future_issuance" [] pause facts 0
  , entry "invalid_future_signature" [] pause { facts with authority.signatureValid := false } 0
  , entry "expired_command" [] pause facts 10
  , { entry "discarded_transaction" [] pause facts 2 with commit := false }
  , entry "lost_ack" paused pause facts 100
  , entry "replay_after_revocation" paused pause { facts with authority.enrollmentFresh := false } 100
  , entry "erased_receipt" (paused ++ [.loseReceipts]) pause facts 3
  , entry "erased_receipt_after_status_round_trip" (resumed ++ [.loseReceipts]) pause facts 4
  , entry "command_id_reuse" paused { pause with digest := 22 } facts 3
  , entry "physical_identity_collision" [] pause { facts with commandIdentityUnique := false } 2
  , entry "collision_preserves_recorded_result" paused pause { facts with commandIdentityUnique := false } 3
  , entry "recreated_session_header" [] pause { facts with sessionDocument := "recreated-header" } 2
  ]

theorem replay_after_status_round_trip_cannot_pause_again :
    ((cases[14]!).after.1.goal.execution.goal.status,
      (cases[14]!).after.2.map (·.outcome)) = (.active, some .rejected) := by
  native_decide

theorem lost_ack_returns_the_original_outcome :
    (cases[11]!).after = ((cases[11]!).before, some ⟨10, 20, .applied⟩) := by
  native_decide

def receiptJson (r : Receipt) : String :=
  "{\"command_id\":" ++ toString r.commandId ++ ",\"digest\":" ++ toString r.digest ++
    ",\"outcome\":" ++ jsonString (if r.outcome = .applied then "applied" else "rejected") ++ "}"

def stateJson (s : State) : String :=
  "{\"goal\":" ++ GoalClientControlContracts.snapshotJson s.goal ++
    ",\"receipts\":" ++ jsonArray (s.receipts.map receiptJson) ++ "}"

def commandJson (c : Command) : String :=
  "{\"id\":" ++ toString c.id ++ ",\"digest\":" ++ toString c.digest ++
    ",\"has_peer\":" ++ (if c.hasPeer then "true" else "false") ++
    ",\"session_document\":" ++ jsonString c.sessionDocument ++
    ",\"issued_at\":" ++ toString c.issuedAt ++ ",\"expires_at\":" ++ toString c.expiresAt ++
    ",\"expected\":" ++ GoalClientControlContracts.revisionJson c.expected ++
    ",\"operation\":" ++ GoalClientControlContracts.operationJson c.operation ++ "}"

def factsJson (f : Facts) : String :=
  "{\"authority\":{\"signature_valid\":" ++ (if f.authority.signatureValid then "true" else "false") ++
    ",\"session_owned\":" ++ (if f.authority.sessionOwned then "true" else "false") ++
    ",\"requester_is_node\":" ++ (if f.authority.requesterIsNode then "true" else "false") ++
    ",\"enrollment_fresh\":" ++ (if f.authority.enrollmentFresh then "true" else "false") ++
    ",\"route_applied\":" ++ (if f.authority.routeApplied then "true" else "false") ++ "}" ++
    ",\"command_identity_unique\":" ++ (if f.commandIdentityUnique then "true" else "false") ++
    ",\"session_document\":" ++ jsonString f.sessionDocument ++
    ",\"control\":" ++ GoalClientControlContracts.factsJson
      ⟨true, f.unfinishedRequest, f.resume⟩ ++ "}"

def stepJson : Step → String
  | .loseReceipts => "{\"kind\":\"lose_receipts\"}"
  | .apply c f now next commit =>
      "{\"kind\":\"apply\",\"command\":" ++ commandJson c ++
        ",\"facts\":" ++ factsJson f ++ ",\"now\":" ++ toString now ++
        ",\"next_composite\":" ++ jsonString next ++
        ",\"commit\":" ++ (if commit then "true" else "false") ++ "}"

def caseJson (c : Case) : String :=
  "{\"name\":" ++ jsonString c.name ++ ",\"initial\":" ++ stateJson initial ++
    ",\"history\":" ++ jsonArray (c.history.map stepJson) ++
    ",\"before\":" ++ stateJson c.before ++ ",\"command\":" ++ commandJson c.command ++
    ",\"facts\":" ++ factsJson c.facts ++ ",\"now\":" ++ toString c.now ++
    ",\"next_composite\":" ++ jsonString c.next ++
    ",\"commit\":" ++ (if c.commit then "true" else "false") ++
    ",\"expected\":" ++ stateJson c.after.1 ++ ",\"receipt\":" ++
      (match c.after.2 with | none => "null" | some r => receiptJson r) ++
    ",\"recovered\":" ++
      (match recoverReceipt c.before c.command c.facts.commandIdentityUnique with
       | none => "null" | some r => receiptJson r) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

def localFacts (facts : Facts) : Facts :=
  { facts with authority.requesterIsNode := true }

def localStep : Step → Step
  | .loseReceipts => .loseReceipts
  | .apply command facts now next commit =>
      .apply { command with hasPeer := false } (localFacts facts) now next commit

def localReceiptCases : List Case :=
  (cases.filter fun c => c.name ∈ ["fresh_pause", "lost_ack", "erased_receipt",
    "erased_receipt_after_status_round_trip"]).map fun c =>
      { c with
        name := "local_" ++ c.name
        history := c.history.map localStep
        command := { c.command with hasPeer := false }
        facts := localFacts c.facts }

def receiptCasesJson : String := jsonArray (localReceiptCases.map caseJson)

example : localReceiptCases.length = 4 := by native_decide

end Conformance.GoalClientMutationContracts
