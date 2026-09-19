import Proofs.AgentSession
import Proofs.SessionFork
import Proofs.SessionRecovery

/-! Executable session/fork witnesses for the next conformance layer. These use
actual owners, including negative branches, rather than asserting copied counts. -/
namespace Conformance.SessionDocuments
open AgentSession

def scope : Scope := ⟨1, 10, some 2⟩
def session : Document :=
  { scope := scope
    behavior := 3
    createdAt := 1
    tags := ["review", "session-test"]
    provenance := some
      { task := some 4
        graphRun := some 5
        parentRequestDoc := some 6
        fork := some ⟨7, 2⟩ } }
def request (id time : Nat) : RequestFact :=
  { scope, behavior := 3, createdAt := time, observed := ⟨id + 100, id, .failed⟩ }
def old := request 1 2
def newerRequest := request 2 2

def runtimeControl := { newerRequest with scope := { scope with requester := some 1 } }
example : preserveControlSession session runtimeControl scope true = some session := by decide
example : preserveControlSession session runtimeControl scope false = none := by decide
example : preserveControlSession session runtimeControl { scope with session := 99 } true = none := by decide
example : preserveControlSession session runtimeControl { scope with requester := some 99 } true = none := by decide
example : preserveControlSession session { runtimeControl with behavior := 99 } scope true = none := by decide
example : preserveControlSession session { runtimeControl with scope := { scope with agent := 99 } } scope true = none := by decide

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
example : refresh indexed [{ completedRequest with behavior := 99 }]
    newerRequest.observed 4 = indexed := by decide

def retryState : SessionState :=
  { sessionId := 10
    behaviorId := 3
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

def copied := copyPrefix history child remap childKey 1
example : publish history scope child remap childKey 1 true true true = some copied := by decide
example : copied.messages.map (·.header.refs) = [[⟨101, 0⟩]] := by decide
example : copied.messages.map (·.header.origin) = [some 1] := by decide
example : copied.messages.map (·.header.request) = [none] := by decide
example : copied.messages.map (·.blocks) = history.messages.map (·.blocks) := by decide
example : copied.messages.map (·.nativeId) = history.messages.map (·.nativeId) := by decide
example : copied.messages.map (·.key) = ["child:origin-1"] := by decide
example : copied.compactions = [⟨1004, child.session, 1, 0⟩] := by decide
example : (retentionDependencies copied).originMessages = [1] := by decide
example : (retentionDependencies copied).closingRecords = [101] := by decide
example : publish history scope { child with requester := none } remap childKey 1 true true true = none := by decide
example : publish history scope child remap childKey 1 true false true = none := by decide
example : publish history scope child remap childKey 1 true true false = none := by decide
example : publish history scope child remap childKey 1 false true true = none := by decide
example : (copyPrefix history child remap childKey 0) = ⟨[], []⟩ := by decide
example : publish { history with compactions := [⟨4, scope.session, 2, 0⟩] }
    scope child remap childKey 1 true true true = none := by decide
example : publish { history with compactions := [⟨4, scope.session, 1, 99⟩] }
    scope child remap childKey 100 true true true = none := by decide

def duplicateSequenceHistory : History :=
  { messages := [sourceMessage 1 0, sourceMessage 2 0], compactions := [] }
example : publish duplicateSequenceHistory scope child remap childKey 1 true true true = none := by decide
example : publish history scope child (fun _ => 1000) childKey 1 true true true = none := by decide

end Conformance.SessionDocuments
