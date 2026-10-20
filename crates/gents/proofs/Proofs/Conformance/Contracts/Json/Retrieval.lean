import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Retrieval.Cases

namespace Conformance.Contracts

open Retrieval Retrieval.ContractCases

def natListJson (values : List Nat) : String :=
  jsonArray (values.map toString)

def retrievalMessageJson (m : Message) : String :=
  "{\"sequence\":" ++ toString m.sequence
    ++ ",\"settled\":" ++ (if m.settled then "true" else "false") ++ "}"

def sessionIndexJson (st : SessionIndex) : String :=
  "{\"indexed\":" ++ natListJson st.indexed
    ++ ",\"watermark\":" ++ toString st.watermark ++ "}"

def retrievalIndexCaseJson (c : IndexCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"messages\":" ++ jsonArray (c.messages.map retrievalMessageJson) ++ ","
    ++ "\"state\":" ++ sessionIndexJson c.state ++ ","
    ++ "\"next\":" ++ sessionIndexJson c.step.next ++ ","
    ++ "\"index\":" ++ natListJson c.step.index ++ ","
    ++ "\"delete\":" ++ natListJson c.step.delete ++ ","
    ++ "\"pending_after\":" ++ toString c.pendingAfter
  ++ "}"

def retrievalCandidateJson (c : Candidate) : String :=
  "{\"ref\":" ++ toString c.ref ++ ",\"score\":" ++ toString c.score ++ "}"

def retrievalHitCaseJson (c : HitCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"candidates\":" ++ jsonArray (c.candidates.map retrievalCandidateJson) ++ ","
    ++ "\"authorized\":" ++ natListJson c.authorized ++ ","
    ++ "\"limit\":" ++ toString c.limit ++ ","
    ++ "\"hits\":" ++ jsonArray (c.hits.map retrievalCandidateJson)
  ++ "}"

def retrievalIndexCasesJson : String :=
  jsonArray (indexCases.map retrievalIndexCaseJson)

def retrievalHitCasesJson : String :=
  jsonArray (hitCases.map retrievalHitCaseJson)

end Conformance.Contracts
