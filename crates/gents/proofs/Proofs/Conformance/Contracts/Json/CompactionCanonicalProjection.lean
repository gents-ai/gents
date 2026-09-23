import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.CompactionCanonicalProjection

namespace Conformance.Contracts

open Conformance.ContractCases.CompactionCanonicalProjection

private def natArray (values : List Nat) : String := jsonArray (values.map toString)

private def nativeArray (values : List CanonicalOutput.ReconstructedMessage) : String :=
  jsonArray (values.map reconstructedMessageJson)
private def optionalNativeArray : Option (List CanonicalOutput.ReconstructedMessage) → String
  | none => "null" | some values => nativeArray values

def compactionCanonicalProjectionCaseJson (w : CanonicalCase) : String :=
  let result := canonicalRun w
  let rebuilt := rebuiltNative result
  "{\"name\":" ++ jsonString w.name ++ ",\"input\":{" ++
    "\"session\":" ++ toString w.world.sessionId ++ ",\"request\":" ++ toString w.world.requestId ++
    ",\"segments\":" ++ jsonArray (w.world.segments.map canonicalSegmentJson) ++
    ",\"messages\":" ++ jsonArray (w.world.messages.map canonicalMessageJson) ++
    ",\"source\":" ++ natArray w.source ++
    ",\"context_window\":" ++ toString w.contextWindow ++
    ",\"threshold_basis_points\":" ++ toString w.thresholdBasisPoints ++
    ",\"configured_max_output_tokens\":" ++ toString w.configuredMaxOutputTokens ++
    ",\"can_fit\":" ++ (if w.canFit then "true" else "false") ++
    ",\"prefix_length\":" ++ toString w.prefixLength ++
    ",\"checkpoint\":" ++ toString w.checkpoint ++
    ",\"initial_estimate_tokens\":" ++ toString w.initialEstimateTokens ++
    ",\"rebuilt_estimate_tokens\":" ++ toString w.rebuiltEstimateTokens ++ "}," ++
    "\"expected\":{\"initial_native\":" ++ optionalNativeArray (canonicalInputNative w) ++
    ",\"rebuilt_checkpoint\":" ++ jsonOptionalNat (rebuilt.map (·.1)) ++
    ",\"rebuilt_native\":" ++ optionalNativeArray (rebuilt.map (·.2)) ++
    ",\"result\":" ++ jsonString (canonicalResultName result) ++
    ",\"output_tokens\":" ++ jsonOptionalNat (canonicalOutputTokens result) ++ "}}"

def compactionCanonicalProjectionCasesJson : String :=
  jsonArray (canonicalCases.map compactionCanonicalProjectionCaseJson)

def repairedProjectionAdmissionCaseJson (w : FreshCase) : String :=
  let result := freshResult w
  "{\"name\":" ++ jsonString w.name ++ ",\"input\":{" ++
    "\"source\":" ++ natArray w.source ++
    ",\"project_succeeds\":" ++ (if w.projectSucceeds then "true" else "false") ++
    ",\"estimate_tokens\":" ++ jsonOptionalNat w.estimateTokens ++
    ",\"effective_input_budget\":" ++ toString w.effectiveInputBudget ++
    ",\"context_window\":" ++ toString w.contextWindow ++
    ",\"configured_max_output_tokens\":" ++ toString w.configuredMaxOutputTokens ++ "}," ++
    "\"expected\":{\"result\":" ++ jsonString (freshResultName result) ++
    ",\"output_tokens\":" ++ jsonOptionalNat (freshOutput result) ++
    ",\"projected_source\":" ++ (match freshProjectedSource result with
      | none => "null" | some source => natArray source) ++ "}}"

def repairedProjectionAdmissionCasesJson : String :=
  jsonArray (freshCases.map repairedProjectionAdmissionCaseJson)

end Conformance.Contracts
