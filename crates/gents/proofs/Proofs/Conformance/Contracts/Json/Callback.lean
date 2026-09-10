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
    ++ "\"owner_agent_did\":" ++ jsonString witness.ownerAgentDid ++ ","
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
    ++ ",\"owner_agent_did\":" ++ jsonString inv.ownerAgentDid
    ++ ",\"input\":" ++ jsonString inv.input
    ++ ",\"origin_group_key\":" ++
      (inv.originGroupKey.map Conformance.EventGroupContracts.keyJson).getD "null"
    ++ ",\"state\":" ++ jsonString inv.state.toDefraDB
    ++ ",\"journal\":" ++ jsonArray (inv.journal.map (fun e =>
      "{\"index\":" ++ toString e.index ++ ",\"state\":" ++ jsonString e.state.toDefraDB ++ "}"))
    ++ ",\"result_emitted\":" ++ boolString inv.resultEmitted ++ "}"

def callbackTransitionCasesJson : String :=
  jsonArray (Callback.Conformance.transitionCases.map fun c =>
    "{\"name\":" ++ jsonString c.name ++ ",\"pre\":" ++ callbackInvocationJson c.pre
      ++ ",\"post\":" ++ callbackInvocationJson c.post ++ "}")

def callbackTransitionCaseCount : Nat := Callback.Conformance.transitionCases.length

end Conformance.Contracts
