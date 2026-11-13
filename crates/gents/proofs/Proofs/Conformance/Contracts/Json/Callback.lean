import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Automation.Callback.Conformance
import Proofs.Conformance.EventGroups
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

def callbackCaseJson (witness : Callback.Conformance.CallbackCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"invocation_id\":" ++ jsonString witness.invocationId ++ ","
    ++ "\"owner_node_did\":" ++ jsonString witness.ownerNodeDid ++ ","
    ++ "\"agent_did\":" ++ jsonString witness.agentDid ++ ","
    ++ "\"state\":" ++ jsonString witness.state.toDefraDB ++ ","
    ++ "\"journal\":"
      ++ jsonStringArray (witness.journal.map ActionJournalState.toDefraDB) ++ ","
    ++ "\"journal_prefix_legal\":"
      ++ boolString (CallbackInvocation.journalPrefixOk witness.invocation.journal) ++ ","
    ++ "\"result_emitted\":" ++ boolString witness.resultEmitted ++ ","
    ++ "\"legal\":" ++ boolString witness.legal
    ++ "}"

def callbackCasesJson : String :=
  jsonArray (Callback.Conformance.callbackCases.map callbackCaseJson)

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

def callbackRetryCasesJson : String :=
  jsonArray (Callback.Conformance.retryCases.map fun c =>
    "{\"name\":" ++ jsonString c.name
      ++ ",\"state\":" ++ jsonString c.state.toDefraDB
      ++ ",\"journal\":" ++ jsonStringArray (c.journal.map ActionJournalState.toDefraDB)
      ++ ",\"attempts\":" ++ toString c.attempts
      ++ ",\"max_attempts\":" ++ toString c.maxAttempts
      ++ ",\"allowed\":" ++ boolString c.allowed ++ "}")

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

def callbackTransitionCaseCount : Nat := Callback.Conformance.transitionCases.length

end Conformance.Contracts
