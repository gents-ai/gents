import Proofs.PromptAssembly.State

/-! The configured threshold arrives as an exact integer budget; production
owns the floating-point percentage conversion in
`provider_input::budget::threshold_budget`. -/

namespace PromptAssembly.Budget

/-- Natural subtraction deliberately matches Rust's `saturating_sub`; `min`
keeps the result beneath the operator's configured output ceiling. -/
def effectiveOutputBudget
    (inputTokens contextWindow configuredMaxOutputTokens : Nat) : Nat :=
  min configuredMaxOutputTokens (contextWindow - inputTokens)

/-- Strict positivity is required: when `inputTokens ≥ contextWindow`,
natural subtraction saturates to zero and the old non-strict safety predicate
admitted a wire request with `max_tokens = 0` (#719). -/
def CanDispatch
    (inputTokens contextWindow configuredMaxOutputTokens : Nat) : Prop :=
  0 < effectiveOutputBudget inputTokens contextWindow configuredMaxOutputTokens

instance (inputTokens contextWindow configuredMaxOutputTokens : Nat) :
    Decidable (CanDispatch inputTokens contextWindow configuredMaxOutputTokens) := by
  unfold CanDispatch
  infer_instance

def configuredThresholdBudget (contextWindow basisPoints : Nat) : Nat :=
  contextWindow * basisPoints / 10000

def effectiveInputBudget
    (configuredThresholdBudget contextWindow : Nat) : Nat :=
  min configuredThresholdBudget contextWindow

/-- The recovery band: four fifths of the effective input budget. The whole
request right after a reduction fits it, so the next turns grow back toward
the threshold instead of crossing it again straight away. -/
def recoveryBandBudget (effectiveInputBudget : Nat) : Nat :=
  4 * effectiveInputBudget / 5

/-- The retained tail after a reduction: the configured keep-recent cap,
else what the recovery band leaves after the fixed provider-input layers and
the rolling checkpoint's reserve (the summary call's output cap). Natural
subtraction matches Rust's saturating subtraction. -/
def compactionRetentionTarget
    (configuredKeepRecent effectiveInputBudget fixedInput summaryReserve : Nat) : Nat :=
  min configuredKeepRecent (recoveryBandBudget effectiveInputBudget - fixedInput - summaryReserve)

def summaryOutputCeiling
    (configuredMaxOutputTokens contextWindow : Nat) : Nat :=
  min configuredMaxOutputTokens (max 1 ((contextWindow + 3) / 4))

def rollingSummaryInputBudget
    (contextWindow effectiveMaxOutputTokens : Nat) : Nat :=
  if effectiveMaxOutputTokens < contextWindow
  then contextWindow - effectiveMaxOutputTokens
  else contextWindow

theorem compaction_retention_le_configured
    (configuredKeepRecent effectiveInputBudget fixedInput summaryReserve : Nat) :
    compactionRetentionTarget configuredKeepRecent effectiveInputBudget fixedInput
        summaryReserve ≤ configuredKeepRecent := by
  exact Nat.min_le_left _ _

theorem compaction_retention_le_available
    (configuredKeepRecent effectiveInputBudget fixedInput summaryReserve : Nat) :
    compactionRetentionTarget configuredKeepRecent effectiveInputBudget fixedInput
        summaryReserve ≤ effectiveInputBudget - fixedInput := by
  unfold compactionRetentionTarget recoveryBandBudget
  have htarget : min configuredKeepRecent
      (4 * effectiveInputBudget / 5 - fixedInput - summaryReserve) ≤
      4 * effectiveInputBudget / 5 - fixedInput - summaryReserve := Nat.min_le_right _ _
  omega

/-- The recovery band invariant: when the fixed layers and the checkpoint's
reserve fit the band, the whole post-compaction request (fixed layers,
checkpoint and retained tail) is at most four fifths of the budget. -/
theorem compaction_post_request_within_recovery_band
    (configuredKeepRecent effectiveInputBudget fixedInput summaryReserve : Nat)
    (hfits : fixedInput + summaryReserve ≤ recoveryBandBudget effectiveInputBudget) :
    fixedInput + summaryReserve +
        compactionRetentionTarget configuredKeepRecent effectiveInputBudget fixedInput
          summaryReserve ≤
      recoveryBandBudget effectiveInputBudget := by
  unfold compactionRetentionTarget
  have htarget : min configuredKeepRecent
      (recoveryBandBudget effectiveInputBudget - fixedInput - summaryReserve) ≤
      recoveryBandBudget effectiveInputBudget - fixedInput - summaryReserve :=
    Nat.min_le_right _ _
  omega

/-- When nothing binds but the band, the retained tail is exactly what the
band leaves: the band is filled, not under-used. -/
theorem compaction_retention_fills_recovery_band
    (effectiveInputBudget fixedInput summaryReserve : Nat)
    (hfits : fixedInput + summaryReserve ≤ recoveryBandBudget effectiveInputBudget) :
    fixedInput + summaryReserve +
        compactionRetentionTarget (recoveryBandBudget effectiveInputBudget)
          effectiveInputBudget fixedInput summaryReserve =
      recoveryBandBudget effectiveInputBudget := by
  unfold compactionRetentionTarget
  have hle : recoveryBandBudget effectiveInputBudget - fixedInput - summaryReserve ≤
      recoveryBandBudget effectiveInputBudget := by omega
  rw [Nat.min_eq_right hle]
  omega

theorem rolling_summary_preserves_configured_output
    {inputTokens contextWindow effectiveMaxOutputTokens : Nat}
    (hceiling : effectiveMaxOutputTokens < contextWindow)
    (hinput : inputTokens ≤
      rollingSummaryInputBudget contextWindow effectiveMaxOutputTokens) :
    effectiveOutputBudget inputTokens contextWindow effectiveMaxOutputTokens =
      effectiveMaxOutputTokens := by
  unfold rollingSummaryInputBudget at hinput
  rw [if_pos hceiling] at hinput
  unfold effectiveOutputBudget
  apply Nat.min_eq_left
  omega

inductive ThresholdDecision where
  | notNeeded
  | reduceEligible
  deriving DecidableEq, Repr

def decideThreshold (inputTokens effectiveInputBudget : Nat) : ThresholdDecision :=
  if inputTokens ≤ effectiveInputBudget then .notNeeded else .reduceEligible

def ExceedsInputBudget
    (promptTokens requestTokens configuredThresholdBudget contextWindow : Nat) : Prop :=
  decideThreshold (promptTokens + requestTokens)
    (effectiveInputBudget configuredThresholdBudget contextWindow) = .reduceEligible

instance (promptTokens requestTokens configuredThresholdBudget contextWindow : Nat) :
    Decidable (ExceedsInputBudget promptTokens requestTokens configuredThresholdBudget
      contextWindow) := by
  unfold ExceedsInputBudget
  infer_instance

theorem exceeds_input_budget_iff
    (promptTokens requestTokens configuredThresholdBudget contextWindow : Nat) :
    ExceedsInputBudget promptTokens requestTokens configuredThresholdBudget contextWindow ↔
      effectiveInputBudget configuredThresholdBudget contextWindow <
        promptTokens + requestTokens := by
  simp [ExceedsInputBudget, decideThreshold, Nat.not_le]

theorem effective_input_le_configured
    (configuredThresholdBudget contextWindow : Nat) :
    effectiveInputBudget configuredThresholdBudget contextWindow ≤
      configuredThresholdBudget := by
  exact Nat.min_le_left _ _

theorem effective_input_le_context
    (configuredThresholdBudget contextWindow : Nat) :
    effectiveInputBudget configuredThresholdBudget contextWindow ≤ contextWindow := by
  exact Nat.min_le_right _ _

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

theorem positive_capacity_can_dispatch
    {inputTokens contextWindow configuredMaxOutputTokens : Nat}
    (hinput : inputTokens < contextWindow)
    (houtput : 0 < configuredMaxOutputTokens) :
    CanDispatch inputTokens contextWindow configuredMaxOutputTokens := by
  simp [CanDispatch, effectiveOutputBudget, houtput,
    Nat.sub_pos_iff_lt.mpr hinput]

theorem can_dispatch_has_positive_capacity
    {inputTokens contextWindow configuredMaxOutputTokens : Nat}
    (hdispatch : CanDispatch inputTokens contextWindow configuredMaxOutputTokens) :
    0 < configuredMaxOutputTokens ∧ inputTokens < contextWindow := by
  constructor
  · exact lt_of_lt_of_le hdispatch (Nat.min_le_left _ _)
  · exact Nat.sub_pos_iff_lt.mp
      (lt_of_lt_of_le hdispatch (Nat.min_le_right _ _))

theorem can_dispatch_is_provider_safe
    {inputTokens contextWindow configuredMaxOutputTokens : Nat}
    (hdispatch : CanDispatch inputTokens contextWindow configuredMaxOutputTokens) :
    inputTokens + effectiveOutputBudget inputTokens contextWindow
      configuredMaxOutputTokens ≤ contextWindow := by
  exact dynamic_output_is_provider_safe
    (Nat.le_of_lt (can_dispatch_has_positive_capacity hdispatch).2)

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

theorem not_exceeds_is_provider_safe
    {promptTokens requestTokens configuredThresholdBudget contextWindow
      configuredMaxOutputTokens : Nat}
    (hnot : ¬ ExceedsInputBudget promptTokens requestTokens configuredThresholdBudget
      contextWindow) :
    promptTokens + requestTokens +
      effectiveOutputBudget (promptTokens + requestTokens) contextWindow
        configuredMaxOutputTokens ≤ contextWindow := by
  apply within_effective_is_provider_safe
  rw [exceeds_input_budget_iff] at hnot
  exact Nat.le_of_not_gt hnot

/-- `run_loop_stream` can issue several provider completions for one durable
request, and tool calls grow the provider input between them, so checking the
budget only at durable-request entry is insufficient: the dispatch guard is a
per-turn obligation. -/

abbrev TurnInputs := List Nat

def EveryDispatchedTurnSafe
    (inputs : TurnInputs) (configuredThresholdBudget contextWindow
      configuredMaxOutputTokens : Nat) : Prop :=
  ∀ inputTokens ∈ inputs,
    ¬ ExceedsInputBudget inputTokens 0 configuredThresholdBudget contextWindow →
    CanDispatch inputTokens contextWindow configuredMaxOutputTokens →
    inputTokens + effectiveOutputBudget inputTokens contextWindow
      configuredMaxOutputTokens ≤ contextWindow

theorem every_dispatched_turn_is_provider_safe
    {inputs : TurnInputs} {configuredThresholdBudget contextWindow
      configuredMaxOutputTokens : Nat} :
    EveryDispatchedTurnSafe inputs configuredThresholdBudget contextWindow
      configuredMaxOutputTokens := by
  intro inputTokens _ _ hdispatch
  exact can_dispatch_is_provider_safe hdispatch

end PromptAssembly.Budget
