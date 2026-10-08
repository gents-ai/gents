import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Callback.Conformance
import Proofs.Conformance.EventGroups

namespace Conformance.Contracts

open Conformance.ContractCases

def callbackCaseJson (witness : Callback.Conformance.CallbackCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"invocation_id\":" ++ jsonString witness.invocationId ++ ","
    ++ "\"owner_node_did\":" ++ jsonString witness.ownerNodeDid ++ ","
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

def callbackTransitionCaseCount : Nat := Callback.Conformance.transitionCases.length

end Conformance.Contracts
