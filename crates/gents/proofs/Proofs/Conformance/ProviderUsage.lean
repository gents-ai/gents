import Proofs.InferenceCall.Usage
import Proofs.Conformance.ContractTypes

namespace Conformance.ProviderUsage

open InferenceCall.Usage Conformance.Contracts

structure Case where
  name : String
  observations : List Observation

def base : Observation :=
  { input := some 100, output := some 20, cacheRead := some 64,
    cacheWrite := some 36, reasoning := some 5, providerTotal := some 120,
    finalResponse := true }

def values : List (Option Nat) :=
  [none, some 0, some 1, some 100, some counterLimit, some (counterLimit + 1)]

def componentCases : List Case := values.flatMap fun value =>
  [⟨"input_" ++ toString value, [{ base with input := value }]⟩,
   ⟨"output_" ++ toString value, [{ base with output := value }]⟩,
   ⟨"cache_read_" ++ toString value, [{ base with cacheRead := value }]⟩,
   ⟨"cache_write_" ++ toString value, [{ base with cacheWrite := value }]⟩,
   ⟨"reasoning_" ++ toString value, [{ base with reasoning := value }]⟩,
   ⟨"provider_total_" ++ toString value, [{ base with providerTotal := value }]⟩]

def cases : List Case := componentCases ++
  [⟨"no_calls", []⟩,
   ⟨"wire_response", [base]⟩,
   ⟨"wire_chat", [{ base with cacheWrite := none }]⟩,
   ⟨"wire_chat_missing_output", [{ input := some 100, providerTotal := some 120, finalResponse := true }]⟩,
   ⟨"wire_missing_final", [{}]⟩,
   ⟨"wire_responses_no_details", [{ input := some 100, output := some 20, providerTotal := some 120, finalResponse := true }]⟩,
   ⟨"wire_xai_separate_reasoning", [{ input := some 32, output := some 9, cacheRead := some 8, reasoning := some 110, providerTotal := some 151, finalResponse := true }]⟩,
   ⟨"missing", [{}]⟩,
   ⟨"empty_final_response", [{ finalResponse := true }]⟩,
   ⟨"reported_zero", [{ input := some 0, output := some 0, finalResponse := true }]⟩,
   ⟨"nonfinal_counters", [{ base with finalResponse := false }]⟩,
   ⟨"messages_cumulative", [{ input := some 100, output := some 20, cacheRead := some 64, cacheWrite := some 36, finalResponse := true }]⟩,
   ⟨"messages_eof", [{ input := some 100, output := some 20, cacheRead := some 64, cacheWrite := some 36 }]⟩,
   ⟨"output_not_reported", [{ input := some 100, finalResponse := true }]⟩,
   ⟨"optional_breakdowns_absent", [{ input := some 100, output := some 20, finalResponse := true }]⟩,
   ⟨"provider_total_is_not_a_second_charge", [{ base with providerTotal := some 999 }]⟩,
   ⟨"complete_and_missing_calls", [base, {}, { input := some 7 }]⟩,
   ⟨"equal_counters_from_distinct_calls", [base, base]⟩,
   ⟨"summary_overflow", [{ input := some counterLimit }, { input := some 1 }]⟩]

def optionalNat : Option Nat → String
  | none => "null"
  | some n => toString n

def observationJson (usage : Observation) : String :=
  "{\"input_tokens\":" ++ optionalNat usage.input ++
  ",\"output_tokens\":" ++ optionalNat usage.output ++
  ",\"cached_input_tokens\":" ++ optionalNat usage.cacheRead ++
  ",\"cache_creation_input_tokens\":" ++ optionalNat usage.cacheWrite ++
  ",\"reasoning_output_tokens\":" ++ optionalNat usage.reasoning ++
  ",\"provider_total_tokens\":" ++ optionalNat usage.providerTotal ++
  ",\"final_response\":" ++ toString usage.finalResponse ++ "}"

def completenessJson : Completeness → String
  | .missing => jsonString "missing"
  | .incomplete => jsonString "partial"
  | .complete => jsonString "complete"

def expectedJson (usage : Observation) : String :=
  "{\"valid\":" ++ toString (valid usage) ++
  ",\"completeness\":" ++ completenessJson (completeness usage) ++
  ",\"charged_tokens\":" ++ optionalNat (charged usage) ++
  ",\"for_storage\":" ++ observationJson (forStorage usage) ++
  ",\"separate_reasoning\":" ++ observationJson (withSeparateReasoning usage) ++
  ",\"storage_completeness\":" ++ completenessJson (completeness (forStorage usage)) ++ "}"

def columnJson (column : Column) : String :=
  "{\"known\":" ++ toString column.known ++
  ",\"reported\":" ++ toString column.reported ++ "}"

def summaryJson (summary : Summary) : String :=
  "{\"calls\":" ++ toString summary.calls ++
  ",\"incomplete_calls\":" ++ toString summary.incompleteCalls ++
  ",\"input\":" ++ columnJson summary.input ++
  ",\"output\":" ++ columnJson summary.output ++
  ",\"cache_read\":" ++ columnJson summary.cacheRead ++
  ",\"cache_write\":" ++ columnJson summary.cacheWrite ++
  ",\"reasoning\":" ++ columnJson summary.reasoning ++
  ",\"provider_total\":" ++ columnJson summary.providerTotal ++ "}"

def casesJson : String := jsonArray (cases.map fun c =>
  "{\"name\":" ++ jsonString c.name ++
  ",\"observations\":" ++ jsonArray (c.observations.map observationJson) ++
  ",\"expected\":" ++ jsonArray (c.observations.map expectedJson) ++
  ",\"charged_total\":" ++ optionalNat ((summarize c.observations).bind Summary.chargedTotal) ++
  ",\"fresh_input\":" ++ optionalNat ((summarize c.observations).bind Summary.freshInput) ++
  ",\"summary\":" ++ ((summarize c.observations).map summaryJson).getD "null" ++ "}")

theorem witnesses_keep_zero_distinct_from_missing :
    completeness {} = .missing ∧
    completeness { input := some 0, output := some 0, finalResponse := true } = .complete ∧
    charged { input := some 0, output := some 0, finalResponse := true } = some 0 := by
  native_decide

theorem witnesses_reject_subsets_and_summary_overflow :
    valid { base with cacheRead := some 101 } = false ∧
    summarize [{ input := some counterLimit }, { input := some 1 }] = none := by
  native_decide

end Conformance.ProviderUsage
