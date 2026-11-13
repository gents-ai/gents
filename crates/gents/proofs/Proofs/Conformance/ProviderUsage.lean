import Proofs.Inference.InferenceCall.LimitObservation
import Proofs.Inference.InferenceCall.Usage
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

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

@[conformance_vocabulary]
def completenessName : Completeness → String
  | .missing => "missing"
  | .incomplete => "partial"
  | .complete => "complete"

deriving instance Lean.ToJson for Completeness

structure UsageWire where
  inputTokens : Option Nat
  outputTokens : Option Nat
  cachedInputTokens : Option Nat
  cacheCreationInputTokens : Option Nat
  reasoningOutputTokens : Option Nat
  providerTotalTokens : Option Nat
  finalResponse : Bool
  deriving Lean.ToJson

def UsageWire.ofModel (u : Observation) : UsageWire :=
  { inputTokens := u.input, outputTokens := u.output, cachedInputTokens := u.cacheRead
    cacheCreationInputTokens := u.cacheWrite, reasoningOutputTokens := u.reasoning
    providerTotalTokens := u.providerTotal, finalResponse := u.finalResponse }

def UsageWire.toModel (u : UsageWire) : Observation :=
  { input := u.inputTokens, output := u.outputTokens, cacheRead := u.cachedInputTokens
    cacheWrite := u.cacheCreationInputTokens, reasoning := u.reasoningOutputTokens
    providerTotal := u.providerTotalTokens, finalResponse := u.finalResponse }

structure ObservationOutcome where
  valid : Bool
  completeness : Completeness
  chargedTokens : Option Nat
  forStorage : UsageWire
  separateReasoning : UsageWire
  storageCompleteness : Completeness
  deriving Lean.ToJson

def ObservationOutcome.ofModel (u : Observation) : ObservationOutcome :=
  { valid := InferenceCall.Usage.valid u
    completeness := InferenceCall.Usage.completeness u
    chargedTokens := charged u
    forStorage := .ofModel (InferenceCall.Usage.forStorage u)
    separateReasoning := .ofModel (withSeparateReasoning u)
    storageCompleteness := InferenceCall.Usage.completeness (InferenceCall.Usage.forStorage u) }

structure ColumnWire where
  known : Nat
  reported : Nat
  deriving Lean.ToJson

def ColumnWire.ofModel (c : Column) : ColumnWire := { known := c.known, reported := c.reported }

structure SummaryWire where
  calls : Nat
  incompleteCalls : Nat
  input : ColumnWire
  output : ColumnWire
  cacheRead : ColumnWire
  cacheWrite : ColumnWire
  reasoning : ColumnWire
  providerTotal : ColumnWire
  deriving Lean.ToJson

def SummaryWire.ofModel (s : Summary) : SummaryWire :=
  { calls := s.calls, incompleteCalls := s.incompleteCalls, input := .ofModel s.input
    output := .ofModel s.output, cacheRead := .ofModel s.cacheRead
    cacheWrite := .ofModel s.cacheWrite, reasoning := .ofModel s.reasoning
    providerTotal := .ofModel s.providerTotal }

structure UsageInput where
  observations : List UsageWire
  deriving Lean.ToJson

structure UsageOutput where
  expected : List ObservationOutcome
  chargedTotal : Option Nat
  freshInput : Option Nat
  summary : Option SummaryWire
  deriving Lean.ToJson

@[conformance_family]
def observations : Conformance.Family UsageInput UsageOutput where
  inputs := cases.map fun c => { observations := c.observations.map UsageWire.ofModel }
  model := fun i =>
    let observed := i.observations.map UsageWire.toModel
    { expected := observed.map ObservationOutcome.ofModel
      chargedTotal := (summarize observed).bind Summary.chargedTotal
      freshInput := (summarize observed).bind Summary.freshInput
      summary := (summarize observed).map SummaryWire.ofModel }

structure StoredInput where
  observation : UsageWire
  deriving Lean.ToJson

structure StoredOutput where
  forStorage : UsageWire
  storageCompleteness : Completeness
  incompleteCalls : Option Nat
  chargedTotal : Option Nat
  deriving Lean.ToJson

/-- The invalid observations: a terminal publication must survive a malformed
provider report, so these are the reports an admission permit stores through
`forStorage`. -/
@[conformance_family]
def invalidStorage : Conformance.Family StoredInput StoredOutput where
  inputs := (cases.flatMap (·.observations)).filter (!valid ·) |>.map fun o =>
    { observation := .ofModel o }
  model := fun i =>
    let stored := InferenceCall.Usage.forStorage i.observation.toModel
    { forStorage := .ofModel stored
      storageCompleteness := InferenceCall.Usage.completeness stored
      incompleteCalls := (summarize [stored]).map (·.incompleteCalls)
      chargedTotal := (summarize [stored]).bind Summary.chargedTotal }

theorem witnesses_keep_zero_distinct_from_missing :
    completeness {} = .missing ∧
    completeness { input := some 0, output := some 0, finalResponse := true } = .complete ∧
    charged { input := some 0, output := some 0, finalResponse := true } = some 0 := by
  native_decide

theorem witnesses_reject_subsets_and_summary_overflow :
    valid { base with cacheRead := some 101 } = false ∧
    summarize [{ input := some counterLimit }, { input := some 1 }] = none := by
  native_decide


deriving instance Lean.ToJson for InferenceCall.LimitObservation.Trigger

structure LimitReadInput where
  trigger : InferenceCall.LimitObservation.Trigger
  now : Nat
  previous : Option Nat
  deriving Lean.ToJson

@[conformance_family]
def limitRead : Conformance.Family LimitReadInput Bool where
  inputs :=
    let triggers : List InferenceCall.LimitObservation.Trigger := [.cached, .open, .refresh]
    let times : List (Option Nat) := [none, some 699, some 700, some 701, some 1000, some 1001]
    triggers.flatMap fun trigger => times.map fun previous => { trigger, now := 1000, previous }
  model := fun i =>
    InferenceCall.LimitObservation.shouldRead i.trigger (i.now : Int)
      (i.previous.map fun time => (time : Int))

end Conformance.ProviderUsage
