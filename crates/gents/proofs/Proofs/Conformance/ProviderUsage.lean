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

/-- A refusal as the wire carries it: the parsed fields only, each unknown an
explicit null rather than a zero or a guess. -/
structure RefusalWire where
  observedAt : Int
  window : Option String
  used : Option Nat
  cap : Option Nat
  resetAt : Option Int
  /-- The model the refusal is about, which is part of what it establishes. -/
  model : Option String
  deriving Lean.ToJson

def RefusalWire.ofModel (r : Refusal) : RefusalWire :=
  { observedAt := r.observedAt, window := r.window, used := r.used, cap := r.cap
    resetAt := r.resetAt, model := r.model }

def RefusalWire.toModel (r : RefusalWire) : Refusal :=
  { observedAt := r.observedAt, window := r.window, used := r.used, cap := r.cap
    resetAt := r.resetAt, model := r.model }

/-- One call's evidence: when it happened, whether it completed, which model it
ran on, and the refusal it stated. -/
structure CallWire where
  observedAt : Int
  completed : Bool
  model : Option String
  refusal : Option RefusalWire
  deriving Lean.ToJson

def CallWire.ofModel (c : CallFact) : CallWire :=
  { observedAt := c.observedAt, completed := c.completed, model := c.model
    refusal := c.refusal.map RefusalWire.ofModel }

def CallWire.toModel (c : CallWire) : CallFact :=
  { observedAt := c.observedAt, completed := c.completed, model := c.model
    refusal := c.refusal.map RefusalWire.toModel }

structure RefusalInput where
  calls : List CallWire
  now : Int
  deriving Lean.ToJson

structure RefusalOutput where
  standing : Option RefusalWire
  /-- Exhaustion is a standing refusal, never a percentage: the view reports
  the same predicate. -/
  exhausted : Bool
  deriving Lean.ToJson

/-- One case: the evidence a backend's calls left, and the clock. -/
structure RefusalCase where
  name : String
  calls : List CallFact
  now : Int

/-- The model xAI's own refusal names. -/
def grokModel : Option String := some "grok-4.7"

/-- A sibling model of the same account. -/
def siblingModel : Option String := some "grok-4.7-mini"

/-- The Grok-family refusal the item exists for: a stated window and counter,
and a reset the provider never gave. -/
def grokAt (moment : Int) : CallFact :=
  { observedAt := moment, model := grokModel, refusal := some
      { observedAt := moment, window := some "rolling 24-hour"
        used := some 691791, cap := some 600000, model := grokModel } }

/-- A refusal that stated nothing beyond itself: no window, no counter, no cap,
no reset. Every unknown stays unknown. -/
def bareAt (moment : Int) : CallFact :=
  { observedAt := moment, refusal := some { observedAt := moment } }

/-- A refusal that stated an absolute reset. -/
def resettingAt (moment : Int) (reset : Option Int) : CallFact :=
  { observedAt := moment, model := grokModel,
    refusal := some { observedAt := moment, resetAt := reset, model := grokModel } }

def grok : CallFact := grokAt 100

def bare : CallFact := bareAt 100

def resetting (reset : Option Int) : CallFact := resettingAt 100 reset

/-- A call that completed at that moment, on the refusing model. `at` is a Lean
keyword. -/
def success (moment : Int) : CallFact :=
  { observedAt := moment, completed := true, model := grokModel }

/-- A call that completed on a sibling model of the same account. -/
def successElsewhere (moment : Int) : CallFact :=
  { observedAt := moment, completed := true, model := siblingModel }

/-- A refusal on a named model, stating nothing else. -/
def refusalAt (moment : Int) (model : Option String) : CallFact :=
  { observedAt := moment, model := model,
    refusal := some { observedAt := moment, model := model } }

/-- A call that finished without stating a refusal. -/
def quiet (moment : Int) : CallFact := { observedAt := moment }

/-- The rules in the order they apply, including the two boundaries that decide
whether a completion clears: strictly after clears, at the same moment does not.
The evidence list is oldest first; the query's own window is the caller's. -/
def refusalCases : List RefusalCase :=
  [⟨"no_evidence", [], 200⟩,
   ⟨"only_completions", [success 90, success 95], 200⟩,
   ⟨"silent_failures", [quiet 90, quiet 95], 200⟩,
   ⟨"grok_unstated_reset", [grok], 200⟩,
   ⟨"bare_refusal", [bare], 200⟩,
   ⟨"later_completion_clears", [grok, success 150], 200⟩,
   ⟨"earlier_completion_does_not", [success 50, grok], 200⟩,
   ⟨"completion_at_the_same_moment_does_not", [grok, success 100], 200⟩,
   ⟨"newer_refusal_replaces_older", [bareAt 80, grokAt 120], 200⟩,
   ⟨"older_refusal_does_not_replace_newer", [grokAt 120, bareAt 80], 200⟩,
   ⟨"stated_reset_passed", [resetting (some 150)], 200⟩,
   ⟨"stated_reset_ahead", [resetting (some 250)], 200⟩,
   ⟨"reset_exactly_now_ends_it", [resetting (some 200)], 200⟩,
   ⟨"completion_before_a_stated_reset_clears", [resetting (some 250), success 150], 200⟩,
   ⟨"refusal_cleared_then_a_newer_one_stands", [grokAt 80, success 150, bareAt 180], 200⟩,
   ⟨"newest_refusal_is_the_one_with_a_reset", [bareAt 80, resettingAt 120 (some 150)], 200⟩,
   ⟨"superseded_refusal_reset_is_forgotten", [resettingAt 80 (some 150), bareAt 120], 200⟩,
   ⟨"malformed_pair_is_still_a_refusal", [bare, quiet 120], 200⟩,
   ⟨"future_clock_keeps_a_stale_refusal", [grok], 50⟩,
   -- Model scope: a provider's refusal is about one model, so a completion on a
   -- sibling model is evidence about that model and not this one.
   ⟨"sibling_model_success_does_not_clear", [grok, successElsewhere 150], 200⟩,
   ⟨"same_model_success_still_clears", [grok, success 150], 200⟩,
   ⟨"sibling_completion_before_the_refusal_is_irrelevant",
     [successElsewhere 50, grok], 200⟩,
   -- Per scope: the NEWER refusal cleared on its own model must not hide the
   -- OLDER one still standing on another. grok-4.7 refuses at 80 and is cleared
   -- at 150; grok-4.7-mini refuses at 120 and is untouched.
   ⟨"cleared_newer_refusal_does_not_hide_an_older_standing_one",
     [grokAt 80, grokAt 120, success 150], 200⟩,
   ⟨"older_refusal_still_stands_when_the_newer_is_cleared",
     [refusalAt 80 siblingModel, grokAt 120, success 150], 200⟩]

@[conformance_family]
def refusalStanding : Conformance.Family RefusalInput RefusalOutput where
  inputs := refusalCases.map fun c =>
    { calls := c.calls.map CallWire.ofModel, now := c.now }
  model := fun i =>
    let established := InferenceCall.Usage.standingRefusal (i.calls.map CallWire.toModel) i.now
    { standing := established.map RefusalWire.ofModel
      exhausted := established.isSome }

end Conformance.ProviderUsage
