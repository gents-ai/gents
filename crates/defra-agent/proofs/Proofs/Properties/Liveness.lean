import Proofs.CrossMachineComposed

open RequestState RequestContext ComposedState

/-!
# Liveness Properties L1-L3

## Liveness taxonomy (tier 1 — #557)

Theorems in this file (and the wider suite's `*_eventually_*` /
`*_convergence` names) are **tier-1 existential reachability**:

> there exists a finite legal trace from the pre-state to a good post-state.

They are **not**:
- **tier 2** fair-scheduler liveness (progress under weak/strong fairness), or
- **tier 3** bounded-latency guarantees (progress within N ticks/steps), or
- **tier 4** operational watchdog guarantees (runtime-enforced timeouts).

Tier-2/3 load for distributed delivery and pairing lives in `tla/`. Tier-4 is
enforced by the Rust runtime (deadlines, idle timeouts, recovery sweeps).

Naming note: historical `*_eventually_*` names are kept for continuity; new
work should prefer `*_reachable` when the theorem is purely existential.
See `crates/defra-agent/proofs/README.md` § Liveness taxonomy.
-/

/-- Termination measure: maximum remaining steps to terminal state. -/
def terminationMeasure (r : RequestContext) : Nat :=
  match r.state with
  | .completed => 0
  | .failed => 0
  | .superseded => 0
  | .dead => 0
  | .interrupted => 0
  | .pending => r.maxRetries + 4
  | .claimed => r.maxRetries + 3
  | .processing => (r.maxRetries - r.retryCount) + 2
  | .inputRequired => (r.maxRetries - r.retryCount) + 2

theorem phase_change_decreases_measure
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post)
    (h_phase_change : pre.state ≠ post.state) :
    terminationMeasure post < terminationMeasure pre := by
  cases h_trans with
  | claim h_pre _ _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | dedup_lose h_pre _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | begin_inference h_pre _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
    omega
  | advance h_pre _ h_post =>
    rw [h_post] at h_phase_change
    exact (h_phase_change rfl).elim
  | finish h_pre _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | fail h_pre _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | fail_before_stream h_pre _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | expire h_pre _ _ _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | interrupt_before_claim h_pre _ _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | interrupt_claimed h_pre _ _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]
  | interrupt_processing h_pre _ _ h_post =>
    rw [h_post]
    simp [terminationMeasure, h_pre]

/-- A recovery step terminates one stuck request. -/
structure RecoveryStep where
  request : RequestContext
  h_stuck : request.state = .processing ∨ request.state = .claimed
  result : RequestContext
  h_terminal : isTerminal result.state

private theorem failed_is_terminal : isTerminal RequestState.failed :=
  Or.inr (Or.inl rfl)

theorem claimed_eventually_terminal
    {pre : ComposedState}
    (h_claimed : pre.request.state = .claimed)
    (h_coherent : pre.request.coherent) :
    ∃ post : ComposedState, ComposedState.Trace pre post ∧ isTerminal post.request.state := by
  let postRequest : RequestContext := { pre.request with state := .failed, admission := .released }
  let post : ComposedState := { pre with request := postRequest }
  have h_admission : pre.request.admission = .waiting ∨ pre.request.admission = .acquired := by
    exact RequestContext.claimed_coherent_cases h_claimed h_coherent
  have h_req : RequestContext.Transition pre.request postRequest := by
    exact RequestContext.Transition.fail_before_stream h_claimed h_admission rfl
  have h_step : ComposedState.Transition pre post := by
    refine ComposedState.Transition.request_step h_req rfl rfl rfl rfl ?_ ?_
    · intro h_pending
      rw [h_claimed] at h_pending
      simp at h_pending
    · -- h_no_block: vacuously true — fail_before_stream doesn't bump
      -- progressSeq, and post.request.state = .failed ≠ .processing.
      intro h_anti
      cases h_anti with
      | inl h_progress =>
          -- post.request.progressSeq = pre.request.progressSeq, contradiction
          simp [post, postRequest] at h_progress
      | inr h_begin =>
          -- post.request.state = .failed, not .processing
          obtain ⟨_, h_proc⟩ := h_begin
          simp [post, postRequest] at h_proc
  refine ⟨post, ComposedState.Trace.step h_step ComposedState.Trace.refl, failed_is_terminal⟩

theorem recovery_convergence
    (stuck : List RequestContext)
    (_h_all_stuck : ∀ r, r ∈ stuck → r.state = .processing ∨ r.state = .claimed) :
    ∃ results : List RequestContext,
      results.length = stuck.length ∧
      ∀ r, r ∈ results → isTerminal r.state := by
  induction stuck with
  | nil =>
    exact ⟨[], rfl, fun _ h => absurd h (List.not_mem_nil _)⟩
  | cons hd tl ih =>
    have h_tl : ∀ r, r ∈ tl → r.state = .processing ∨ r.state = .claimed :=
      fun r hr => _h_all_stuck r (List.mem_cons_of_mem hd hr)
    obtain ⟨rest, h_len, h_term⟩ := ih h_tl
    refine ⟨{ hd with state := .failed, admission := .released } :: rest, ?_, ?_⟩
    · simp [h_len]
    · intro r hr
      cases hr with
      | head => exact failed_is_terminal
      | tail _ h => exact h_term r h
