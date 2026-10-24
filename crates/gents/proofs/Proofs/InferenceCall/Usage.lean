import Proofs.Basic

namespace InferenceCall.Usage

/-- Input includes cache reads and cache writes; reasoning is a subset of
output. Adapters normalize provider-specific counters before this boundary.
An absent counter is unreported, including when another counter is zero. -/
structure Observation where
  input : Option Nat := none
  output : Option Nat := none
  cacheRead : Option Nat := none
  cacheWrite : Option Nat := none
  reasoning : Option Nat := none
  providerTotal : Option Nat := none
  finalResponse : Bool := false
  deriving DecidableEq, Repr

def counterLimit : Nat := 9223372036854775807

def bounded (value : Option Nat) : Bool :=
  value.all (· ≤ counterLimit)

def subset (part whole : Option Nat) : Bool :=
  match part, whole with
  | some part, some whole => part ≤ whole
  | _, _ => true

def valid (usage : Observation) : Bool :=
  bounded usage.input && bounded usage.output && bounded usage.cacheRead &&
    bounded usage.cacheWrite && bounded usage.reasoning && bounded usage.providerTotal &&
    subset usage.cacheRead usage.input && subset usage.cacheWrite usage.input &&
    subset usage.reasoning usage.output &&
    match usage.input, usage.cacheRead, usage.cacheWrite with
    | some input, some read, some write => read + write ≤ input
    | _, _, _ => true

inductive Completeness where
  | missing | incomplete | complete
  deriving DecidableEq, Repr

def hasCounters (usage : Observation) : Bool :=
  usage.input.isSome || usage.output.isSome || usage.cacheRead.isSome ||
    usage.cacheWrite.isSome || usage.reasoning.isSome || usage.providerTotal.isSome

/-- A terminal lifecycle is not proof of a final provider usage event. Optional
breakdowns need not be reported for the input/output total to be complete. -/
def completeness (usage : Observation) : Completeness :=
  if !hasCounters usage then .missing
  else if usage.finalResponse && usage.input.isSome && usage.output.isSome then .complete
  else .incomplete

def charged (usage : Observation) : Option Nat :=
  if valid usage && completeness usage == .complete then
    match usage.input, usage.output with
    | some input, some output => some (input + output)
    | _, _ => none
  else none

/-- xAI's Chat usage reports visible completion tokens separately from
reasoning tokens (https://docs.x.ai/developers/tools/tool-usage-details).
Both reported parts are required to establish total output at this boundary. -/
def withSeparateReasoning (usage : Observation) : Observation :=
  { usage with output := match usage.output, usage.reasoning with
      | some output, some reasoning => some (output + reasoning)
      | _, _ => none }

/-- A malformed provider usage report must not prevent the call's terminal
publication. Retain representable core counters, discard contradictory
breakdowns, and never advertise invalid usage as complete. Counters outside
the durable integer range remain unreported rather than being clamped. -/
def forStorage (usage : Observation) : Observation :=
  let clipped : Observation :=
    { input := usage.input.filter (· ≤ counterLimit)
      output := usage.output.filter (· ≤ counterLimit)
      cacheRead := usage.cacheRead.filter (· ≤ counterLimit)
      cacheWrite := usage.cacheWrite.filter (· ≤ counterLimit)
      reasoning := usage.reasoning.filter (· ≤ counterLimit)
      providerTotal := usage.providerTotal.filter (· ≤ counterLimit)
      finalResponse := usage.finalResponse && valid usage }
  if valid clipped then clipped
  else { clipped with cacheRead := none, cacheWrite := none, reasoning := none }

private theorem bounded_filter (value : Option Nat) :
    bounded (value.filter (· ≤ counterLimit)) = true := by
  cases value with
  | none => rfl
  | some n =>
    by_cases h : n ≤ counterLimit <;> simp [bounded, Option.filter, h]

theorem storage_is_valid (usage : Observation) : valid (forStorage usage) = true := by
  unfold forStorage
  dsimp only
  split
  · assumption
  · have hi := bounded_filter usage.input
    have ho := bounded_filter usage.output
    have ht := bounded_filter usage.providerTotal
    simp only [bounded] at hi ho ht
    simp [valid, subset, bounded, hi, ho, ht]

structure Column where
  known : Nat := 0
  reported : Nat := 0
  deriving DecidableEq, Repr

def Column.add (column : Column) (value : Option Nat) : Column :=
  match value with
  | none => column
  | some value => ⟨column.known + value, column.reported + 1⟩

structure Summary where
  calls : Nat := 0
  incompleteCalls : Nat := 0
  input : Column := {}
  output : Column := {}
  cacheRead : Column := {}
  cacheWrite : Column := {}
  reasoning : Column := {}
  providerTotal : Column := {}
  deriving DecidableEq, Repr

def Summary.add (summary : Summary) (usage : Observation) : Summary :=
  { calls := summary.calls + 1
    incompleteCalls := summary.incompleteCalls +
      if completeness usage == .complete then 0 else 1
    input := summary.input.add usage.input
    output := summary.output.add usage.output
    cacheRead := summary.cacheRead.add usage.cacheRead
    cacheWrite := summary.cacheWrite.add usage.cacheWrite
    reasoning := summary.reasoning.add usage.reasoning
    providerTotal := summary.providerTotal.add usage.providerTotal }

def Column.valid (column : Column) : Bool :=
  column.known ≤ counterLimit && column.reported ≤ counterLimit

def Summary.valid (summary : Summary) : Bool :=
  summary.calls ≤ counterLimit && summary.incompleteCalls ≤ counterLimit &&
    summary.input.valid && summary.output.valid && summary.cacheRead.valid &&
    summary.cacheWrite.valid && summary.reasoning.valid && summary.providerTotal.valid

def summarize (observations : List Observation) : Option Summary :=
  let summary := observations.foldl Summary.add {}
  if observations.all valid && summary.valid then some summary else none

def Summary.chargedTotal (summary : Summary) : Option Nat :=
  if summary.incompleteCalls = 0 then some (summary.input.known + summary.output.known)
  else none

def Summary.freshInput (summary : Summary) : Option Nat :=
  if summary.input.reported = summary.calls ∧ summary.cacheRead.reported = summary.calls then
    some (summary.input.known - summary.cacheRead.known)
  else none

theorem absent_counter_preserves_column (column : Column) :
    column.add none = column := rfl

theorem zero_is_a_reported_counter (column : Column) :
    (column.add (some 0)).reported = column.reported + 1 := rfl

theorem missing_usage_is_not_chargeable : charged {} = none := by native_decide

theorem partial_response_is_not_chargeable (usage : Observation)
    (h : usage.finalResponse = false) : charged usage = none := by
  cases hc : hasCounters usage <;> simp [charged, completeness, h, hc]

theorem invalid_storage_is_not_chargeable (usage : Observation)
    (h : valid usage = false) : charged (forStorage usage) = none := by
  apply partial_response_is_not_chargeable
  unfold forStorage
  dsimp only
  split <;> simp [h]

theorem provider_total_does_not_choose_the_charge (usage : Observation)
    (total : Option Nat)
    (validBefore : valid usage = true)
    (validAfter : valid { usage with providerTotal := total } = true)
    (components : usage.input.isSome = true ∧ usage.output.isSome = true) :
    charged { usage with providerTotal := total } = charged usage := by
  rcases usage with ⟨input, output, read, write, reasoning, oldTotal, finalResponse⟩
  cases input <;> cases output <;>
    simp_all [charged, completeness, hasCounters]

theorem aggregation_preserves_missing_evidence (summary : Summary) :
    (summary.add {}).incompleteCalls = summary.incompleteCalls + 1 := by
  simp [Summary.add, completeness, hasCounters]

end InferenceCall.Usage
