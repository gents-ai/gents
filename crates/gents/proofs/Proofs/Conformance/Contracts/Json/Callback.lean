import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Automation.Callback.Conformance
import Proofs.Conformance.EventGroups
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

def callbackInvocationJson (inv : CallbackInvocation) : String :=
  "{\"invocation_id\":" ++ jsonString inv.invocationId
    ++ ",\"owner_node_did\":" ++ jsonString inv.ownerNodeDid
    ++ ",\"agent_did\":" ++ jsonString inv.agentDid
    ++ ",\"input\":" ++ jsonString inv.input
    ++ ",\"origin_group_key\":" ++
      (inv.originGroupKey.map Conformance.EventGroupContracts.keyJson).getD "null"
    ++ ",\"state\":" ++ jsonString inv.state.toDefraDB
    ++ ",\"journal\":" ++ jsonArray (inv.journal.map (fun e =>
      "{\"index\":" ++ toString e.index ++ ",\"state\":" ++ jsonString e.state.toDefraDB ++ "}"))
    ++ ",\"result_emitted\":" ++ boolString inv.resultEmitted
    ++ ",\"attempts\":" ++ toString inv.attempts ++ "}"

def callbackTransitionCasesJson : String :=
  jsonArray (Callback.Conformance.transitionCases.map fun c =>
    "{\"name\":" ++ jsonString c.name ++ ",\"pre\":" ++ callbackInvocationJson c.pre
      ++ ",\"post\":" ++ callbackInvocationJson c.post ++ "}")

deriving instance Lean.ToJson for InvocationState, ActionJournalState, ActionJournalEntry

structure RecoveryInput where
  name : String
  journal : List ActionJournalState
  attempts : Nat
  maxAttempts : Nat
  deriving Lean.ToJson

def RecoveryInput.invocation (i : RecoveryInput) : CallbackInvocation :=
  { invocationId := "inv-1", ownerNodeDid := "dep-1", agentDid := "did:key:agent", state := .running,
    journal := Callback.Conformance.numberedJournal i.journal, resultEmitted := false,
    attempts := i.attempts }

def recoveryInputs : List RecoveryInput :=
  Callback.Conformance.recoveryCases.map fun c =>
    { name := c.name, journal := c.journal, attempts := c.attempts, maxAttempts := c.maxAttempts }

structure RecoveryOutput where
  postState : InvocationState
  postJournal : List ActionJournalEntry
  retryAllowedAfter : Bool
  deriving Lean.ToJson

/-- A running invocation after `recover_running` at startup, and whether
`retry_allowed` then permits another attempt. -/
@[conformance_family]
def callbackRecovery : Family RecoveryInput RecoveryOutput where
  inputs := recoveryInputs
  model := fun i =>
    let post := CallbackInvocation.recover i.invocation
    { postState := post.state, postJournal := post.journal
      retryAllowedAfter := CallbackInvocation.retryAllowed post i.maxAttempts }

structure DenialOutput where
  denyPostState : InvocationState
  denyPostJournal : List ActionJournalEntry
  retryAllowedAfterDeny : Bool
  deriving Lean.ToJson

/-- A running invocation after `apply_planner_deny`, and whether
`retry_allowed` then permits another attempt. -/
@[conformance_family]
def callbackDenial : Family RecoveryInput DenialOutput where
  inputs := recoveryInputs
  model := fun i =>
    let denied := CallbackInvocation.deny i.invocation
    { denyPostState := denied.state, denyPostJournal := denied.journal
      retryAllowedAfterDeny := CallbackInvocation.retryAllowed denied i.maxAttempts }

namespace CallbackJournal

open Lean

def journalOf (journal : List String) : List ActionJournalEntry :=
  Callback.Conformance.numberedJournal (journal.filterMap ActionJournalState.fromDefraDB?)

structure RetryInput where
  state : String
  journal : List String
  attempts : Nat
  maxAttempts : Nat
  deriving ToJson

structure JournalInput where
  agentDid : String
  ownerNodeDid : String
  journal : List String
  deriving ToJson

example : Callback.Conformance.callbackCases.all fun c => c.agentDid != c.ownerNodeDid := by native_decide

example : Callback.Conformance.retryCases.any (·.allowed) := by native_decide

end CallbackJournal

@[conformance_family]
def callbackRetry : Family CallbackJournal.RetryInput Bool where
  inputs := Callback.Conformance.retryCases.map fun c =>
    { state := c.state.toDefraDB, journal := c.journal.map ActionJournalState.toDefraDB
      attempts := c.attempts, maxAttempts := c.maxAttempts }
  model := fun i =>
    CallbackInvocation.retryAllowed
      { invocationId := "inv-1", ownerNodeDid := "dep-1", agentDid := "did:key:agent"
        state := (InvocationState.fromDefraDB? i.state).getD .pending
        journal := CallbackJournal.journalOf i.journal, resultEmitted := false
        attempts := i.attempts }
      i.maxAttempts

@[conformance_family]
def callbackJournal : Family CallbackJournal.JournalInput Bool where
  inputs := Callback.Conformance.callbackCases.map fun c =>
    { agentDid := c.agentDid, ownerNodeDid := c.ownerNodeDid
      journal := c.journal.map ActionJournalState.toDefraDB }
  model := fun i => CallbackInvocation.journalPrefixOk (CallbackJournal.journalOf i.journal)


end Conformance.Contracts
