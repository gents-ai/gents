import Proofs.PromptAssembly.State

/-!
# Provider input budgeting

The provider must fit the assembled input and the requested output inside one
context window. The configured output value is a ceiling, not a reservation:
each completion clamps that ceiling to the space left after its assembled
input. Compaction can therefore follow the configured history threshold while
the per-turn output clamp preserves provider safety.

The configured threshold is represented as an already-computed token budget.
Production owns the floating-point percentage conversion; the contract cases
emit exact percentage-derived budgets and fence that conversion in Rust.
-/

namespace PromptAssembly.Budget

/-- Output tokens available on one provider turn after its assembled input.
Natural subtraction deliberately matches Rust's `saturating_sub`; `min` keeps
the result beneath the operator's configured output ceiling. -/
def effectiveOutputBudget
    (inputTokens contextWindow configuredMaxOutputTokens : Nat) : Nat :=
  min configuredMaxOutputTokens (contextWindow - inputTokens)

/-- A provider attempt is legal only when the assembled request both fits the
context and retains strictly positive output capacity. Merely proving the
non-strict context inequality is insufficient: when `inputTokens ≥
contextWindow`, natural subtraction saturates to zero and the old safety
predicate admitted a wire request with `max_tokens = 0` (#719). -/
def CanDispatch
    (inputTokens contextWindow configuredMaxOutputTokens : Nat) : Prop :=
  0 < effectiveOutputBudget inputTokens contextWindow configuredMaxOutputTokens

instance (inputTokens contextWindow configuredMaxOutputTokens : Nat) :
    Decidable (CanDispatch inputTokens contextWindow configuredMaxOutputTokens) := by
  unfold CanDispatch
  infer_instance

/-- The operator's configured share of the context window, in exact integer
arithmetic over basis points.

The configuration surface carries the threshold as a float, but the budget is
*computed* from basis points on both sides (`compaction::threshold_budget`), so
this model is exact for every threshold rather than only for those exactly
representable in binary. Computing it as `contextWindow × threshold` in floating
point and truncating disagrees with this for e.g. 57% of 10,000 — 5,699 rather
than 5,700 (#1008). -/
def configuredThresholdBudget (contextWindow basisPoints : Nat) : Nat :=
  contextWindow * basisPoints / 10000

/-- The input limit that triggers compaction. The configured threshold remains
authoritative, capped by the context window for malformed configuration. Output
space is enforced separately by `effectiveOutputBudget` on every turn. -/
def effectiveInputBudget
    (configuredThresholdBudget contextWindow : Nat) : Nat :=
  min configuredThresholdBudget contextWindow

/-- The assembled prompt and the incoming request no longer fit beneath the
effective input budget. Equality is admitted; one token beyond it compacts. -/
def ExceedsInputBudget
    (promptTokens requestTokens configuredThresholdBudget contextWindow : Nat) : Prop :=
  effectiveInputBudget configuredThresholdBudget contextWindow <
    promptTokens + requestTokens

instance (promptTokens requestTokens configuredThresholdBudget contextWindow : Nat) :
    Decidable (ExceedsInputBudget promptTokens requestTokens configuredThresholdBudget
      contextWindow) := by
  unfold ExceedsInputBudget
  infer_instance

theorem effective_input_le_configured
    (configuredThresholdBudget contextWindow : Nat) :
    effectiveInputBudget configuredThresholdBudget contextWindow ≤
      configuredThresholdBudget := by
  exact Nat.min_le_left _ _

theorem effective_input_le_context
    (configuredThresholdBudget contextWindow : Nat) :
    effectiveInputBudget configuredThresholdBudget contextWindow ≤ contextWindow := by
  exact Nat.min_le_right _ _

/-- Clamping the configured output ceiling to the context remaining after this
turn's input guarantees provider safety. -/
theorem dynamic_output_is_provider_safe
    {inputTokens contextWindow configuredMaxOutputTokens : Nat}
    (hinput : inputTokens ≤ contextWindow) :
    inputTokens + effectiveOutputBudget inputTokens contextWindow
      configuredMaxOutputTokens ≤ contextWindow := by
  calc
    inputTokens + effectiveOutputBudget inputTokens contextWindow
        configuredMaxOutputTokens ≤ inputTokens + (contextWindow - inputTokens) :=
      Nat.add_le_add_left (Nat.min_le_right _ _) inputTokens
    _ = contextWindow := Nat.add_sub_of_le hinput

/-- Positive configured output and at least one token of remaining context are
exactly the missing premises needed to construct a legal provider dispatch. -/
theorem positive_capacity_can_dispatch
    {inputTokens contextWindow configuredMaxOutputTokens : Nat}
    (hinput : inputTokens < contextWindow)
    (houtput : 0 < configuredMaxOutputTokens) :
    CanDispatch inputTokens contextWindow configuredMaxOutputTokens := by
  simp [CanDispatch, effectiveOutputBudget, houtput,
    Nat.sub_pos_iff_lt.mpr hinput]

/-- Conversely, a legal dispatch witnesses both a positive configured ceiling
and strict room beyond the assembled input. -/
theorem can_dispatch_has_positive_capacity
    {inputTokens contextWindow configuredMaxOutputTokens : Nat}
    (hdispatch : CanDispatch inputTokens contextWindow configuredMaxOutputTokens) :
    0 < configuredMaxOutputTokens ∧ inputTokens < contextWindow := by
  constructor
  · exact lt_of_lt_of_le hdispatch (Nat.min_le_left _ _)
  · exact Nat.sub_pos_iff_lt.mp
      (lt_of_lt_of_le hdispatch (Nat.min_le_right _ _))

/-- Dispatch legality strengthens the old non-strict context inequality: every
legal request is provider-safe, but a zero-output request is not legal merely
because that inequality happens to hold. -/
theorem can_dispatch_is_provider_safe
    {inputTokens contextWindow configuredMaxOutputTokens : Nat}
    (hdispatch : CanDispatch inputTokens contextWindow configuredMaxOutputTokens) :
    inputTokens + effectiveOutputBudget inputTokens contextWindow
      configuredMaxOutputTokens ≤ contextWindow := by
  exact dynamic_output_is_provider_safe
    (Nat.le_of_lt (can_dispatch_has_positive_capacity hdispatch).2)

/-- Staying beneath the effective input budget makes the turn's input fit the
context, after which the dynamic output clamp makes the complete provider
request safe. -/
theorem within_effective_is_provider_safe
    {promptTokens requestTokens configuredThresholdBudget contextWindow
      configuredMaxOutputTokens : Nat}
    (hwithin : promptTokens + requestTokens ≤
      effectiveInputBudget configuredThresholdBudget contextWindow) :
    promptTokens + requestTokens +
      effectiveOutputBudget (promptTokens + requestTokens) contextWindow
        configuredMaxOutputTokens ≤ contextWindow := by
  exact dynamic_output_is_provider_safe
    (hwithin.trans (effective_input_le_context configuredThresholdBudget contextWindow))

/-- If compaction is not required, the provider-safety theorem applies. -/
theorem not_exceeds_is_provider_safe
    {promptTokens requestTokens configuredThresholdBudget contextWindow
      configuredMaxOutputTokens : Nat}
    (hnot : ¬ ExceedsInputBudget promptTokens requestTokens configuredThresholdBudget
      contextWindow) :
    promptTokens + requestTokens +
      effectiveOutputBudget (promptTokens + requestTokens) contextWindow
        configuredMaxOutputTokens ≤ contextWindow := by
  exact within_effective_is_provider_safe (Nat.le_of_not_gt hnot)

/-! ## Owned-loop turn safety

`run_loop_stream` can issue several provider completions for one durable
request. Tool calls and their results grow the provider input between those
completions, so checking the budget only when the durable request enters the
daemon is insufficient. The dispatch guard is a per-turn obligation: every
turn that reaches the provider must have passed the same output-reserved gate.
-/

/-- The input-token estimates observed at each completion turn. -/
abbrev TurnInputs := List Nat

/-- Every completion turn which the runtime elects to dispatch is provider
safe. Membership makes the quantification explicitly range over the entire
owned-loop trace rather than only its first input. -/
def EveryDispatchedTurnSafe
    (inputs : TurnInputs) (configuredThresholdBudget contextWindow
      configuredMaxOutputTokens : Nat) : Prop :=
  ∀ inputTokens ∈ inputs,
    ¬ ExceedsInputBudget inputTokens 0 configuredThresholdBudget contextWindow →
    CanDispatch inputTokens contextWindow configuredMaxOutputTokens →
    inputTokens + effectiveOutputBudget inputTokens contextWindow
      configuredMaxOutputTokens ≤ contextWindow

/-- Applying the input guard and dynamic output clamp before every completion
dispatch makes the whole owned-loop trace safe, even when later turns grow
beyond the entry turn's budget. -/
theorem every_dispatched_turn_is_provider_safe
    {inputs : TurnInputs} {configuredThresholdBudget contextWindow
      configuredMaxOutputTokens : Nat} :
    EveryDispatchedTurnSafe inputs configuredThresholdBudget contextWindow
      configuredMaxOutputTokens := by
  intro inputTokens _ _ hdispatch
  exact can_dispatch_is_provider_safe hdispatch

end PromptAssembly.Budget
