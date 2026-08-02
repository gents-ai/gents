import Proofs.PromptAssembly.State

/-!
# Provider input budgeting

The provider must fit the assembled input and the requested output inside one
context window. Compaction therefore starts at the stricter of the configured
history threshold and the space left after reserving the requested output.

The configured threshold is represented as an already-computed token budget.
Production owns the floating-point percentage conversion; the contract cases
emit exact percentage-derived budgets and fence that conversion in Rust.
-/

namespace PromptAssembly.Budget

/-- Tokens available to provider input after reserving the requested output.
Natural subtraction deliberately matches Rust's `saturating_sub`. -/
def providerInputBudget (contextWindow maxOutputTokens : Nat) : Nat :=
  contextWindow - maxOutputTokens

/-- The input limit that triggers compaction. Neither the operator's configured
threshold nor the provider's output reservation may be exceeded. -/
def effectiveInputBudget
    (configuredThresholdBudget contextWindow maxOutputTokens : Nat) : Nat :=
  min configuredThresholdBudget (providerInputBudget contextWindow maxOutputTokens)

/-- The assembled prompt and the incoming request no longer fit beneath the
effective input budget. Equality is admitted; one token beyond it compacts. -/
def ExceedsInputBudget
    (promptTokens requestTokens configuredThresholdBudget contextWindow
      maxOutputTokens : Nat) : Prop :=
  effectiveInputBudget configuredThresholdBudget contextWindow maxOutputTokens <
    promptTokens + requestTokens

instance (promptTokens requestTokens configuredThresholdBudget contextWindow
    maxOutputTokens : Nat) :
    Decidable (ExceedsInputBudget promptTokens requestTokens configuredThresholdBudget
      contextWindow maxOutputTokens) := by
  unfold ExceedsInputBudget
  infer_instance

theorem effective_le_configured
    (configuredThresholdBudget contextWindow maxOutputTokens : Nat) :
    effectiveInputBudget configuredThresholdBudget contextWindow maxOutputTokens ≤
      configuredThresholdBudget := by
  exact Nat.min_le_left _ _

theorem effective_le_provider_input
    (configuredThresholdBudget contextWindow maxOutputTokens : Nat) :
    effectiveInputBudget configuredThresholdBudget contextWindow maxOutputTokens ≤
      providerInputBudget contextWindow maxOutputTokens := by
  exact Nat.min_le_right _ _

/-- Staying beneath the effective budget guarantees that input plus the output
reservation fits the provider context. This is the safety property the old
percentage-only trigger violated for large output reservations. -/
theorem within_effective_is_provider_safe
    {promptTokens requestTokens configuredThresholdBudget contextWindow
      maxOutputTokens : Nat}
    (houtput : maxOutputTokens ≤ contextWindow)
    (hwithin : promptTokens + requestTokens ≤
      effectiveInputBudget configuredThresholdBudget contextWindow maxOutputTokens) :
    promptTokens + requestTokens + maxOutputTokens ≤ contextWindow := by
  calc
    promptTokens + requestTokens + maxOutputTokens ≤
        providerInputBudget contextWindow maxOutputTokens + maxOutputTokens :=
      Nat.add_le_add_right
        (hwithin.trans
          (effective_le_provider_input configuredThresholdBudget contextWindow
            maxOutputTokens))
        maxOutputTokens
    _ = contextWindow := Nat.sub_add_cancel houtput

/-- If compaction is not required, the provider-safety theorem applies. -/
theorem not_exceeds_is_provider_safe
    {promptTokens requestTokens configuredThresholdBudget contextWindow
      maxOutputTokens : Nat}
    (houtput : maxOutputTokens ≤ contextWindow)
    (hnot : ¬ ExceedsInputBudget promptTokens requestTokens configuredThresholdBudget
      contextWindow maxOutputTokens) :
    promptTokens + requestTokens + maxOutputTokens ≤ contextWindow := by
  exact within_effective_is_provider_safe houtput (Nat.le_of_not_gt hnot)

end PromptAssembly.Budget
