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

def callbackRecoveryCasesJson : String :=
  jsonArray (Callback.Conformance.recoveryCases.map fun c =>
    "{\"name\":" ++ jsonString c.name
      ++ ",\"journal\":" ++ jsonStringArray (c.journal.map ActionJournalState.toDefraDB)
      ++ ",\"attempts\":" ++ toString c.attempts
      ++ ",\"max_attempts\":" ++ toString c.maxAttempts
      ++ ",\"post_state\":" ++ jsonString c.post.state.toDefraDB
      ++ ",\"post_journal\":"
        ++ jsonStringArray (c.post.journal.map fun e => e.state.toDefraDB)
      ++ ",\"retry_allowed_after\":" ++ boolString c.retryAllowedAfter
      ++ ",\"deny_post_state\":" ++ jsonString c.denied.state.toDefraDB
      ++ ",\"deny_post_journal\":"
        ++ jsonStringArray (c.denied.journal.map fun e => e.state.toDefraDB)
      ++ ",\"retry_allowed_after_deny\":" ++ boolString c.retryAllowedAfterDeny ++ "}")

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

def callbackTransitionCaseCount : Nat := Callback.Conformance.transitionCases.length

end Conformance.Contracts
