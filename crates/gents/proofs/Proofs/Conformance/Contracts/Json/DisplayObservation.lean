import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Compaction.DisplayObservation

namespace Conformance.Contracts

open Compaction.DisplayObservation
open Conformance.ContractCases

def displayObservationCaseJson (witness : ObservationCase) : String :=
  "{" ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome.toContract ++ ","
    ++ "\"owner_node\":" ++ toString witness.ownerNode ++ ","
    ++ "\"owner\":" ++ toString witness.owner ++ ","
    ++ "\"reader_node\":" ++ toString witness.readerNode ++ ","
    ++ "\"reader\":" ++ toString witness.reader ++ ","
    ++ "\"visible\":" ++ boolString witness.expectedVisible ++ "}"

def displayObservationCasesJson : String :=
  jsonArray (observationCases.map displayObservationCaseJson)

def displaySchemaCasesJson : String :=
  jsonArray (schemaCases.map (fun witness =>
    "{" ++ "\"local\":" ++ jsonOptionalNat witness.localVersion ++ ","
      ++ "\"remote\":" ++ jsonOptionalNat witness.remoteVersion ++ ","
      ++ "\"compatible\":" ++ boolString witness.expected ++ "}"))

end Conformance.Contracts
