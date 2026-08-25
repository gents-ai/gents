import Proofs.GraphPipeline
import Proofs.Conformance.ContractTypes

namespace Conformance.GraphPipelineContracts

open Conformance.Contracts

structure ValidationCase where
  name : String
  typesValid : Bool
  topologyValid : Bool
  capabilitiesAuthorized : Bool
  withinBounds : Bool
  expectedValid : Bool
  deriving DecidableEq, Repr

def boolValues : List Bool := [false, true]

def validationCases : List ValidationCase :=
  boolValues.flatMap fun typesValid =>
    boolValues.flatMap fun topologyValid =>
      boolValues.flatMap fun capabilitiesAuthorized =>
        boolValues.map fun withinBounds =>
          { name :=
              "types=" ++ toString typesValid ++
              ",topology=" ++ toString topologyValid ++
              ",authorized=" ++ toString capabilitiesAuthorized ++
              ",bounds=" ++ toString withinBounds
          , typesValid := typesValid
          , topologyValid := topologyValid
          , capabilitiesAuthorized := capabilitiesAuthorized
          , withinBounds := withinBounds
          , expectedValid :=
              typesValid && topologyValid && capabilitiesAuthorized && withinBounds
          }

theorem validationCases_count : validationCases.length = 16 := by native_decide

private def boolJson (value : Bool) : String :=
  if value then "true" else "false"

def validationCaseJson (testCase : ValidationCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString testCase.name ++ ","
    ++ "\"types_valid\":" ++ boolJson testCase.typesValid ++ ","
    ++ "\"topology_valid\":" ++ boolJson testCase.topologyValid ++ ","
    ++ "\"capabilities_authorized\":" ++ boolJson testCase.capabilitiesAuthorized ++ ","
    ++ "\"within_bounds\":" ++ boolJson testCase.withinBounds ++ ","
    ++ "\"expected_valid\":" ++ boolJson testCase.expectedValid
    ++ "}"

def validationCasesJson : String :=
  jsonArray (validationCases.map validationCaseJson)

end Conformance.GraphPipelineContracts
