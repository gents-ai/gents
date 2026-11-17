import Proofs.Shared.Basic

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

/-- Where a limit statement came from. A provider's own words are evidence; a
local sum is an estimate; silence is explicitly unknown. Never inferred from
the other. -/
inductive Provenance where
  | providerReported
  | locallyEstimated
  | unknown
  deriving DecidableEq, Repr

/-- A provider's refusal, as the fields it stated. Every field is optional
because a refusal establishes only what it says: plain "free usage exhausted"
establishes the refusal and the local consumption, and no cap, no remaining
balance and no reset. -/
structure Refusal where
  /-- When the provider stated it. `at` is a Lean keyword, hence the longer name. -/
  observedAt : Int
  window : Option String := none
  used : Option Nat := none
  cap : Option Nat := none
  /-- Absolute only. A relative "in 24 hours" leaves this none, rather than
  becoming a time the provider never stated. -/
  resetAt : Option Int := none
  deriving DecidableEq, Repr

/-- What a refusal establishes, and nothing more. A cap below what was used is
consistent with a provider whose counter and cap are on different scales, so it
is not a contradiction. -/
def Refusal.establishes (refusal : Refusal) : Bool :=
  refusal.observedAt ≥ 0 &&
    match refusal.used, refusal.cap with
    | some used, some cap => used ≤ counterLimit && cap ≤ counterLimit && cap > 0
    | _, _ => true

/-- A refusal stands until its stated reset, or until a later successful call on
the same account. A newer refusal replaces an older one; an older success does
not clear a newer refusal. -/
def Refusal.standsAt (refusal : Refusal) (now : Int) : Bool :=
  match refusal.resetAt with
  | some reset => now < reset
  | none => true

/-- An account's standing limit state: the latest refusal and how much the node
has itself observed. Exhaustion is the refusal, never a percentage. -/
structure LimitState where
  refusal : Option Refusal := none
  consumed : Option Nat := none
  provenance : Provenance := .unknown
  deriving DecidableEq, Repr

/-- Exhausted exactly when a refusal stands. A locally estimated consumption may
annotate it but cannot establish it: reaching 100% of a guessed cap is still a
guess. -/
def LimitState.exhausted (state : LimitState) (now : Int) : Bool :=
  match state.refusal with
  | some refusal => refusal.standsAt now
  | none => false

/-- A later successful call clears the refusal it follows, and only that one. -/
def LimitState.clearedBySuccessAt (state : LimitState) (successAt : Int) : LimitState :=
  match state.refusal with
  | some refusal => if successAt > refusal.observedAt then { state with refusal := none } else state
  | none => state

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

/-- The three rules B8 rests on, stated so the Rust owner replays them. -/

theorem a_standing_refusal_is_exhaustion (state : LimitState) (refusal : Refusal) (now : Int)
    (standing : refusal.standsAt now = true) :
    (LimitState.exhausted { state with refusal := some refusal } now) = true := by
  simp [LimitState.exhausted, standing]

theorem no_refusal_is_never_exhaustion (state : LimitState) (now : Int) :
    LimitState.exhausted { state with refusal := none } now = false := rfl

theorem a_local_estimate_alone_is_never_exhaustion (consumed : Option Nat) (now : Int) :
    LimitState.exhausted { refusal := none, consumed, provenance := .locallyEstimated } now
      = false := rfl

theorem a_reset_clears_the_refusal (refusal : Refusal) (reset now : Int)
    (stated : refusal.resetAt = some reset) (passed : now ≥ reset) :
    refusal.standsAt now = false := by
  simp [Refusal.standsAt, stated, passed]

theorem a_later_success_clears_the_refusal (refusal : Refusal) (successAt : Int)
    (later : successAt > refusal.observedAt) :
    (LimitState.clearedBySuccessAt
      { refusal := some refusal, consumed := none, provenance := .providerReported }
      successAt).refusal = none := by
  simp only [LimitState.clearedBySuccessAt]
  rw [if_pos later]

/-- An earlier success does not clear a refusal that came after it. -/
theorem an_earlier_success_leaves_a_newer_refusal (refusal : Refusal) (successAt : Int)
    (earlier : successAt ≤ refusal.observedAt) :
    (LimitState.clearedBySuccessAt
      { refusal := some refusal, consumed := none, provenance := .providerReported }
      successAt).refusal = some refusal := by
  simp only [LimitState.clearedBySuccessAt]
  rw [if_neg (by omega : ¬ successAt > refusal.observedAt)]

/-- A refusal establishing no reset stands indefinitely: only a later success
clears it, which is why an unstated window is not replaced by a guessed one. -/
theorem an_unstated_reset_stands (refusal : Refusal) (now : Int) (unstated : refusal.resetAt = none) :
    refusal.standsAt now = true := by
  simp [Refusal.standsAt, unstated]

/-- One call's contribution to the refusal rules: when it happened, whether it
completed, and the refusal it stated. A completion states none, so an earlier
success cannot establish a refusal or clear one it followed. -/
structure CallFact where
  observedAt : Int
  completed : Bool := false
  /-- The refusal this call stated, when it stated one. Its own `observedAt`
  is this call's time. -/
  refusal : Option Refusal := none
  deriving DecidableEq, Repr

/-- The newest stated refusal in the evidence: a newer refusal replaces an older
one, and evidence stating none states none. Two refusals at the same moment
are the same refusal restated, so the one later in the evidence is kept. -/
def latestRefusal : List CallFact → Option Refusal
  | [] => none
  | call :: rest =>
    match call.refusal, latestRefusal rest with
    | none, newest => newest
    | some refusal, none => some refusal
    | some refusal, some newest =>
        if refusal.observedAt > newest.observedAt then some refusal else some newest

/-- A completion strictly after the refusal clears it. A completion at the
refusal's own moment is the refusing call's neighbour, not evidence it passed. -/
def CallFact.clears (call : CallFact) (refusal : Refusal) : Bool :=
  call.completed && call.observedAt > refusal.observedAt

/-- The refusal the evidence establishes, if one still stands: the newest stated
refusal, while its own reset has not passed and no call completed after it.
`none` means no refusal stands, never that the account is whole. -/
def standingRefusal (calls : List CallFact) (now : Int) : Option Refusal :=
  match latestRefusal calls with
  | none => none
  | some refusal =>
      if refusal.standsAt now && !calls.any (fun call => call.clears refusal)
      then some refusal else none

theorem no_evidence_establishes_no_refusal (now : Int) : standingRefusal [] now = none := rfl

/-- Evidence whose every candidate has closed establishes no refusal, whatever
it otherwise contains. -/
theorem closed_evidence_establishes_no_refusal (calls : List CallFact) (now : Int)
    (closed : ∀ refusal, latestRefusal calls = some refusal → refusal.standsAt now = false) :
    standingRefusal calls now = none := by
  unfold standingRefusal
  cases current : latestRefusal calls with
  | none => rfl
  | some refusal => simp [closed refusal current]

/-- The refusal the rules establish is the one the provider stated, field for
field: a plain "free usage exhausted" keeps its unknowns unknown rather than
becoming a cap, a counter or a reset. -/
theorem witnesses_the_refusal_rules :
    standingRefusal
        [{ observedAt := 10, refusal := some
            { observedAt := 10, window := some "rolling 24-hour"
              used := some 691791, cap := some 600000 } }] 20
      = some { observedAt := 10, window := some "rolling 24-hour"
               used := some 691791, cap := some 600000 } ∧
    standingRefusal [{ observedAt := 10, refusal := some { observedAt := 10 } }] 20
      = some { observedAt := 10 } ∧
    standingRefusal
        [{ observedAt := 10, refusal := some { observedAt := 10 } },
         { observedAt := 12, completed := true }] 20 = none ∧
    standingRefusal
        [{ observedAt := 8, completed := true },
         { observedAt := 10, refusal := some { observedAt := 10 } }] 20
      = some { observedAt := 10 } ∧
    standingRefusal
        [{ observedAt := 10, refusal := some { observedAt := 10 } },
         { observedAt := 10, completed := true }] 20 = some { observedAt := 10 } ∧
    standingRefusal
        [{ observedAt := 10, refusal := some { observedAt := 10, resetAt := some 15 } }] 20 = none ∧
    standingRefusal
        [{ observedAt := 10, refusal := some { observedAt := 10, resetAt := some 25 } }] 20
      = some { observedAt := 10, resetAt := some 25 } := by
  native_decide

end InferenceCall.Usage
