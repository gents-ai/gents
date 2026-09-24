import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.CompactionProjectionJoin

namespace Conformance.Contracts

open Conformance.ContractCases.CompactionProjectionJoin

private def natArray (values : List Nat) : String := jsonArray (values.map toString)

private def rebuiltInputJson : Option (Nat × List Nat) → String
  | none => "null"
  | some (checkpoint, suffix) => "{\"checkpoint\":" ++ toString checkpoint ++
      ",\"retained_suffix\":" ++ natArray suffix ++ "}"

def compactionProjectionJoinCaseJson (witness : Case) : String :=
  "{" ++ "\"name\":" ++ jsonString witness.name ++
    ",\"source\":" ++ natArray witness.source ++
    ",\"context_window\":" ++ toString witness.contextWindow ++
    ",\"threshold_basis_points\":" ++ toString witness.thresholdBasisPoints ++
    ",\"configured_max_output_tokens\":" ++ toString witness.configuredMaxOutputTokens ++
    ",\"can_fit\":" ++ toString witness.canFit ++
    ",\"prefix_length\":" ++ toString witness.prefixLength ++
    ",\"checkpoint\":" ++ toString witness.checkpoint ++
    ",\"initial_projection_succeeds\":" ++ toString witness.initialProjectionSucceeds ++
    ",\"initial_estimate_tokens\":" ++ jsonOptionalNat witness.initialEstimateTokens ++
    ",\"rebuild_succeeds\":" ++ toString witness.rebuildSucceeds ++
    ",\"rebuilt_estimate_tokens\":" ++ jsonOptionalNat witness.rebuiltEstimateTokens ++
    ",\"initial_projection_input\":" ++ natArray witness.source ++
    ",\"rebuilt_projection_input\":" ++ rebuiltInputJson (rebuiltInput witness) ++
    ",\"result\":" ++ jsonString (resultName (result witness)) ++
    ",\"output_tokens\":" ++ jsonOptionalNat (outputTokens witness) ++ "}"

def compactionProjectionJoinCasesJson : String :=
  jsonArray (cases.map compactionProjectionJoinCaseJson)

end Conformance.Contracts
