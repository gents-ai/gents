import Proofs.AgentSession
import Proofs.SessionFork
import Proofs.SessionRecovery

namespace Conformance.SessionDocuments
open AgentSession

def scope : Scope := ⟨1, 10, some 2⟩
def session : Document :=
  { scope := scope
    agent:= 3
    createdAt := 1
    tags := ["review", "session-test"]
    provenance := some
      { task := some 4
        parentRequestDoc := some 6
        startedBy := some ⟨5, 6, some 7⟩
        fork := some ⟨7, 2⟩ } }
example : starterIndex session = some (5, 6) := by decide
example : startedBy session = some ⟨5, 6, some 7⟩ := by decide
example : ensure? (some session) { session with provenance := none } = some session := by decide
example : starterIndex { session with provenance := none } = none := by decide

def starterReceipt : CreationReceipt :=
  { scope := scope, sessionDoc := 8, agent := 3, openingRequestDoc := 9,
    causalKind := 1, causeBinding := 2,
    starter := some ⟨⟨5, 6, some 7⟩, 11⟩ }
example : verifiedStarter starterReceipt 8 ⟨5, 6, some 7⟩ 11 (5, 6) true = some ⟨5, 6, some 7⟩ := by decide
example : verifiedStarter starterReceipt 9 ⟨5, 6, some 7⟩ 11 (5, 6) true = none := by decide
example : verifiedStarter starterReceipt 8 ⟨5, 6, some 7⟩ 11 (5, 7) true = none := by decide
example : verifiedStarter starterReceipt 8 ⟨5, 7, some 7⟩ 11 (5, 7) true = none := by decide
example : verifiedStarter starterReceipt 8 ⟨5, 6, some 7⟩ 12 (5, 6) true = none := by decide
example : verifiedStarter starterReceipt 8 ⟨5, 6, some 7⟩ 11 (5, 6) false = none := by decide
example : observedStarter (some (5, 6)) (some starterReceipt) 8 (some ⟨5, 6, some 7⟩) (some 11) true = some ⟨5, 6, some 7⟩ := by decide
example : observedStarter (some (5, 6)) (some starterReceipt) 8 none (some 12) true = none := by decide
example : observedStarter none (some starterReceipt) 8 (some ⟨5, 6, some 7⟩) (some 11) true = none := by decide

def request (id time : Nat) : RequestFact :=
  { purpose := .normal, scope, agent:= 3, createdAt := time, observed := ⟨id + 100, id, .failed⟩ }
def old := request 1 2
def newerRequest := request 2 2

def titleRequest := { request 3 99 with purpose := .titleAudit }
example : latest [old, titleRequest] scope.node scope.session none = some old := by decide
example : advance session [old, titleRequest] titleRequest "title" 100 = session := by decide
example : observedRequest [titleRequest] titleRequest.observed = none := by decide

example : latest [newerRequest, old] 1 10 none = some newerRequest := by decide
example : latest [old, newerRequest] 1 10 none = some newerRequest := by decide
example : latest [old] 1 10 (some none) = none := by decide
example : latest [old] 9 10 none = none := by decide
example : latest [old] 1 9 none = none := by decide

def indexed := advance session [old, newerRequest] newerRequest "new" 3
example : (advance indexed [old, newerRequest] old "old" 4) = indexed := by decide
example : refresh indexed [old, newerRequest] old.observed 4 = indexed := by decide
example : refresh indexed [old, newerRequest] { newerRequest.observed with docId := 999 } 4 = indexed := by decide
example : (rename indexed (some ⟨"renamed", .user⟩) 1).observation = indexed.observation := by decide
example : observedRequest [old] newerRequest.observed = none := by decide

def completedRequest := { newerRequest with observed := { newerRequest.observed with state := .completed } }
def processingEvent := { newerRequest with observed := { newerRequest.observed with state := .processing } }
example : (refresh indexed [completedRequest] processingEvent.observed 4).observation.bind (·.latest) =
    some completedRequest.observed := by decide
example : refresh indexed [] newerRequest.observed 4 = indexed := by decide
example : refresh indexed [{ completedRequest with scope := { scope with requester := some 99 } }]
    newerRequest.observed 4 = indexed := by decide
example : refresh indexed [{ completedRequest with agent:= 99 }]
    newerRequest.observed 4 = indexed := by decide

def retryState : SessionState :=
  { sessionId := 10
    agentId := 3
    requestIds := {1}
    latest := 1
    ctx := fun _ =>
      { state := .failed
        origin := .interactive
        admission := .released
        deadline := none
        currentTime := 3
        retryCount := 0
        maxRetries := 3 } }

example : (SessionState.retryFromRows? retryState indexed [old] 101 1 3).isSome = true := by native_decide
example : SessionState.retryFromRows? retryState indexed [old, newerRequest] 101 1 3 = none := by
  rfl
example : SessionState.retryFromRows? retryState indexed [old] 999 1 3 = none := by rfl

def olderExistingCandidate := request 3 0
example : SessionState.retryFromRows? retryState indexed [old, olderExistingCandidate] 101 1 3 = none := by rfl

def scopedSuccessor := request 3 3
def foreignRequester := { request 4 4 with scope := { scope with requester := some 99 } }
def absentScopeSession := { session with scope := { scope with requester := none } }
def absentScopeRequest := { scopedSuccessor with scope := absentScopeSession.scope }
example : (advance indexed [old, newerRequest, scopedSuccessor, foreignRequester]
    scopedSuccessor "candidate" 4).observation.bind (·.latest) = some scopedSuccessor.observed := by decide
example : (advance absentScopeSession [absentScopeRequest, foreignRequester]
    absentScopeRequest "candidate" 4).observation.bind (·.latest) = some absentScopeRequest.observed := by decide

open SessionFork
open CanonicalOutput

def sourceMessage (id sequence : Nat) : MessageEnvelope :=
  { header :=
      { id
      , session := scope.session
      , request := some 101
      , origin := none
      , refs := [⟨100 + id, 0⟩]
      , outcome := .complete
      , role := .assistant
      , publication := .requestExecution 7 }
  , key := "origin-" ++ toString id
  , sequence
  , nativeId := some "native-provider-id"
  , blocks := [.text ⟨⟨100 + id, 0⟩, .full⟩]
  , createdAt := 5 }
def history : History :=
  { messages := [sourceMessage 1 0]
    compactions := [⟨4, scope.session, 1, 0⟩] }
def child : Scope := { scope with session := 20 }
def remap (id : Nat) := id + 1000
def childKey (key : String) := "child:" ++ key
def sourceAuthorization : SessionFork.SourceAuthorization :=
  ⟨scope, history.messages.map (·.header.id), history.compactions.map (·.id)⟩
def wrongAgentAuthorization : SessionFork.SourceAuthorization :=
  ⟨{ scope with node := 99 }, history.messages.map (·.header.id), history.compactions.map (·.id)⟩

def copied := copyPrefix history child remap childKey 1
example : publish history scope child remap childKey 1 sourceAuthorization true true = some copied := by decide
example : copied.messages.map (·.header.refs) = [[⟨101, 0⟩]] := by decide
example : copied.messages.map (·.header.origin) = [some 1] := by decide
example : copied.messages.map (·.header.request) = [none] := by decide
example : copied.messages.map (·.blocks) = history.messages.map (·.blocks) := by decide
example : copied.messages.map (·.nativeId) = history.messages.map (·.nativeId) := by decide
example : copied.messages.map (·.key) = ["child:origin-1"] := by decide
example : copied.compactions = [⟨1004, child.session, 1, 0⟩] := by decide
example : (retentionDependencies copied).originMessages = [1] := by decide
example : (retentionDependencies copied).closingRecords = [101] := by decide
example : publish history scope { child with requester := none } remap childKey 1 sourceAuthorization true true = none := by decide
example : publish history scope child remap childKey 1 sourceAuthorization false true = none := by decide
example : publish history scope child remap childKey 1 sourceAuthorization true false = none := by decide
example : publish history scope child remap childKey 1 wrongAgentAuthorization true true = none := by decide
example : publish history scope { child with node := 99 } remap childKey 1 sourceAuthorization true true = none := by decide
example : (copyPrefix history child remap childKey 0) = ⟨[], []⟩ := by decide
example : publish { history with compactions := [⟨4, scope.session, 2, 0⟩] }
    scope child remap childKey 1 sourceAuthorization true true = none := by decide
example : publish { history with compactions := [⟨4, scope.session, 1, 99⟩] }
    scope child remap childKey 100 sourceAuthorization true true = none := by decide

def duplicateSequenceHistory : History :=
  { messages := [sourceMessage 1 0, sourceMessage 2 0], compactions := [] }
example : publish duplicateSequenceHistory scope child remap childKey 1
    (SessionFork.SourceAuthorization.mk scope
      (duplicateSequenceHistory.messages.map (·.header.id))
      (duplicateSequenceHistory.compactions.map (·.id)))
    true true = none := by decide
example : publish history scope child (fun _ => 1000) childKey 1 sourceAuthorization true true = none := by decide

def editUser (id sequence : Nat) : MessageEnvelope :=
  { sourceMessage id sequence with
    header := { (sourceMessage id sequence).header with role := .user } }

def editHistory : History :=
  { messages := [editUser 1 0, sourceMessage 2 1,
      { editUser 3 2 with blocks := [.toolResult 3 "call" none []] }, editUser 4 3]
    compactions := [] }

structure EditAnchorCase where
  name : String
  source : History
  key : String

def threeUserHistory : History :=
  { messages := [editUser 1 1, editUser 2 2, editUser 3 3]
    compactions := [] }

def editAnchorCases : List EditAnchorCase :=
  [ ⟨"three_user_turns", threeUserHistory, "origin-3"⟩
  , ⟨"first_user_turn", editHistory, "origin-1"⟩
  , ⟨"later_user_turn", editHistory, "origin-4"⟩
  , ⟨"assistant_message", editHistory, "origin-2"⟩
  , ⟨"tool_result_only", editHistory, "origin-3"⟩
  , ⟨"missing_message", editHistory, "missing"⟩
  , ⟨"prefix_is_not_identity", editHistory, "origin-"⟩
  , ⟨"ambiguous_key", { editHistory with messages :=
        editHistory.messages ++ [{ editUser 5 4 with key := "origin-1" }] }, "origin-1"⟩
  , ⟨"empty_user_message", { editHistory with messages :=
        [{ editUser 1 0 with blocks := [] }] }, "origin-1"⟩ ]

example : editCut editHistory "origin-1" = some 0 := by decide
example : editCut editHistory "origin-4" = some 3 := by decide
example : editCut editHistory "origin-3" = none := by decide

structure EditForkCase where
  name : String
  requested : EditForkBinding
  retained : ExistingEditFork

def editForkBinding : EditForkBinding := ⟨7, scope, child, "origin-4"⟩

def editForkCases : List EditForkCase :=
  [ ⟨"new_destination", editForkBinding, .missing⟩
  , ⟨"lost_reply_reuses_child", editForkBinding, .bound editForkBinding⟩
  , ⟨"preexisting_unbound_child", editForkBinding, .unbound⟩
  , ⟨"changed_native_client", { editForkBinding with client := 99 }, .bound editForkBinding⟩
  , ⟨"changed_anchor", { editForkBinding with messageKey := "origin-1" },
      .bound editForkBinding⟩
  , ⟨"changed_source", { editForkBinding with parent := { scope with session := 99 } },
      .bound editForkBinding⟩
  , ⟨"changed_child", { editForkBinding with child := { child with session := 99 } },
      .bound editForkBinding⟩
  , ⟨"foreign_requester", { editForkBinding with child := { child with requester := none } },
      .missing⟩
  , ⟨"foreign_node", { editForkBinding with child := { child with node := 99 } }, .missing⟩
  , ⟨"same_source_and_child", { editForkBinding with child := scope }, .missing⟩
  , ⟨"empty_anchor", { editForkBinding with messageKey := "" }, .missing⟩ ]

end Conformance.SessionDocuments
