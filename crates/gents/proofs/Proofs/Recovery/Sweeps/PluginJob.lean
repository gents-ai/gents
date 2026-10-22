import Proofs.Recovery.Contract

namespace Recovery

/-- One running plugin-job tool-call row as the periodic sweep sees it.
    `terminalChildren` counts the job's children with a durable terminal,
    `observed` those a step has already received, and `live` the children
    still running. `stepNew` counts the new spawns and messages
    of the step that receives the terminals up to `terminalChildren` (the
    stall rule reads them). `deadlineExpired` holds once `max_wall_secs` has passed;
    `available` fails when the tool, plugin, job block or install record is
    gone, or the engine was not installed by an operator. `stepped` holds
    once a step has committed since the row's adoption: a resumed row is
    adopted with its chain's children observed and none live, and adoption
    kicks its first step, so until that step commits the row has not stalled
    and the sweep re-drives the step instead.

    Accepted premise: a peer that never replicates a child's terminal leaves
    the job waiting; `max_wall_secs` bounds that wait. -/
structure PluginJobRow where
  running : Bool
  terminalChildren : Nat
  observed : Nat
  live : Nat
  stepNew : Nat
  deadlineExpired : Bool
  available : Bool
  stepped : Bool
  deriving DecidableEq, Repr

/-- No live child, no unobserved terminal and nothing new from the step that
    received them, after a step since adoption (`plugin_job_stalled`). -/
def pluginJobStalled (r : PluginJobRow) : Prop :=
  r.stepped = true ∧ r.live = 0 ∧ r.observed = r.terminalChildren ∧ r.stepNew = 0

instance (r : PluginJobRow) : Decidable (pluginJobStalled r) := by
  unfold pluginJobStalled
  infer_instance

/-- A running job needs the sweep when a child's terminal is unobserved, its
    deadline passed, it is no longer available, it stalled, or it rests with
    no live child before its first step since adoption. -/
def pluginJobStale (r : PluginJobRow) : Prop :=
  r.running ∧ (r.observed < r.terminalChildren ∨ r.deadlineExpired ∨ !r.available
    ∨ (r.live = 0 ∧ r.observed = r.terminalChildren ∧ (r.stepped = false ∨ r.stepNew = 0)))

instance (r : PluginJobRow) : Decidable (pluginJobStale r) := by
  unfold pluginJobStale
  infer_instance

/-- A job past its deadline, unavailable or stalled is killed and fails
    closed; otherwise one step receives its unobserved terminals, or is the
    first step since adoption. -/
def pluginJobRecover (r : PluginJobRow) : PluginJobRow :=
  if pluginJobStale r then
    if r.deadlineExpired ∨ !r.available ∨ pluginJobStalled r
    then { r with running := false }
    else { r with observed := r.terminalChildren, stepped := true }
  else r

theorem pluginJobRow_stale_recover_changes (row : PluginJobRow)
    (h : pluginJobStale row) : pluginJobRecover row ≠ row := by
  have h_stale := h
  obtain ⟨h_running, h_reason⟩ := h
  unfold pluginJobRecover
  rw [if_pos h_stale]
  split
  · intro h_eq
    have h_field := congrArg PluginJobRow.running h_eq
    simp [h_running] at h_field
  · rename_i h_continue
    intro h_eq
    have h_observed := congrArg PluginJobRow.observed h_eq
    have h_stepped := congrArg PluginJobRow.stepped h_eq
    simp at h_observed h_stepped
    rcases h_reason with h_lt | h_deadline | h_unavailable | ⟨h_live, h_obs, h_quiet⟩
    · omega
    · exact (h_continue (Or.inl h_deadline)).elim
    · exact (h_continue (Or.inr (Or.inl h_unavailable))).elim
    · rcases h_quiet with h_unstepped | h_none
      · simp [h_unstepped] at h_stepped
      · exact (h_continue (Or.inr (Or.inr ⟨h_stepped, h_live, h_obs, h_none⟩))).elim

/-- A running job that stepped since adoption, with no live child, no
    unobserved terminal and nothing new from its last step, fails closed
    (`plugin_job_stalled`). -/
theorem pluginJobRow_stalled_recover_not_running (row : PluginJobRow)
    (h_running : row.running = true) (h_stalled : pluginJobStalled row) :
    (pluginJobRecover row).running = false := by
  obtain ⟨h_stepped, h_live, h_obs, h_none⟩ := h_stalled
  have h_stale : pluginJobStale row :=
    ⟨h_running, Or.inr (Or.inr (Or.inr ⟨h_live, h_obs, Or.inr h_none⟩))⟩
  unfold pluginJobRecover
  rw [if_pos h_stale, if_pos (Or.inr (Or.inr ⟨h_stepped, h_live, h_obs, h_none⟩))]

/-- A row with no live child and every terminal observed that has not
    stepped since adoption (a freshly resumed row) is not killed: the tick
    runs its first step, and only that step's output can stall it. -/
theorem pluginJobRow_unstepped_row_steps (row : PluginJobRow)
    (h_running : row.running = true) (h_deadline : row.deadlineExpired = false)
    (h_available : row.available = true) (h_live : row.live = 0)
    (h_obs : row.observed = row.terminalChildren) (h_unstepped : row.stepped = false) :
    (pluginJobRecover row).running = true ∧ (pluginJobRecover row).stepped = true := by
  have h_stale : pluginJobStale row :=
    ⟨h_running, Or.inr (Or.inr (Or.inr ⟨h_live, h_obs, Or.inl h_unstepped⟩))⟩
  unfold pluginJobRecover
  rw [if_pos h_stale, if_neg]
  · exact ⟨h_running, rfl⟩
  · intro h_kill
    rcases h_kill with h_dl | h_av | ⟨h_st, -⟩
    · simp [h_deadline] at h_dl
    · simp [h_available] at h_av
    · simp [h_unstepped] at h_st

/-- A row recover leaves running was neither past its deadline, unavailable
    nor stalled, and has now received its terminals in a step. It is stale
    again only when that step left it stalled. -/
theorem pluginJobRow_recover_not_stale_same_reason (row : PluginJobRow)
    (h : pluginJobStale row) (h_running : (pluginJobRecover row).running = true) :
    ¬ (pluginJobRecover row).observed < (pluginJobRecover row).terminalChildren ∧
      (pluginJobRecover row).deadlineExpired = false ∧
      (pluginJobRecover row).available = true ∧
      (pluginJobRecover row).stepped = true := by
  unfold pluginJobRecover at h_running ⊢
  rw [if_pos h] at h_running ⊢
  split at h_running
  · simp at h_running
  · rename_i h_continue
    rw [if_neg h_continue]
    refine ⟨Nat.lt_irrefl _, ?_, ?_, rfl⟩
    · show row.deadlineExpired = false
      cases h_deadline : row.deadlineExpired
      · rfl
      · exact (h_continue (Or.inl h_deadline)).elim
    · show row.available = true
      cases h_available : row.available
      · exact (h_continue (Or.inr (Or.inl (by rw [h_available]; rfl)))).elim
      · rfl

/-- A row that needs no recovery is left untouched. -/
theorem pluginJobRow_not_stale_unchanged (row : PluginJobRow)
    (h : ¬ pluginJobStale row) : pluginJobRecover row = row := by
  unfold pluginJobRecover
  rw [if_neg h]

/-- A sweep tick re-drives one step and then applies the stall rule to its
    output, so it leaves no row stale. -/
theorem pluginJobRow_tick_not_stale (row : PluginJobRow) (h : pluginJobStale row) :
    ¬ pluginJobStale (pluginJobRecover (pluginJobRecover row)) := by
  by_cases h_again : pluginJobStale (pluginJobRecover row)
  · have h_next := h_again
    obtain ⟨h_running, h_reason⟩ := h_next
    obtain ⟨h_observed, h_deadline, h_available, h_stepped⟩ :=
      pluginJobRow_recover_not_stale_same_reason row h h_running
    have h_stalled : pluginJobStalled (pluginJobRecover row) := by
      rcases h_reason with h_lt | h_dl | h_av | ⟨h_live, h_obs, h_quiet⟩
      · exact absurd h_lt h_observed
      · simp [h_deadline] at h_dl
      · simp [h_available] at h_av
      · rcases h_quiet with h_unstepped | h_none
        · simp [h_stepped] at h_unstepped
        · exact ⟨h_stepped, h_live, h_obs, h_none⟩
    intro h_stale
    obtain ⟨h_run, -⟩ := h_stale
    rw [pluginJobRow_stalled_recover_not_running _ h_running h_stalled] at h_run
    simp at h_run
  · rw [pluginJobRow_not_stale_unchanged _ h_again]
    exact h_again

/-- A tick whose re-driven step emits a new spawn or message
    leaves the job running, even when no child is live. -/
theorem pluginJobRow_tick_keeps_job_with_new_effects (row : PluginJobRow)
    (h_running : row.running = true) (h_unobserved : row.observed < row.terminalChildren)
    (h_deadline : row.deadlineExpired = false) (h_available : row.available = true)
    (h_new : 0 < row.stepNew) :
    (pluginJobRecover (pluginJobRecover row)).running = true := by
  have h_stale : pluginJobStale row := ⟨h_running, Or.inl h_unobserved⟩
  have h_first : pluginJobRecover row =
      { row with observed := row.terminalChildren, stepped := true } := by
    unfold pluginJobRecover
    rw [if_pos h_stale, if_neg]
    intro h_kill
    rcases h_kill with h_dl | h_av | ⟨-, -, h_obs, -⟩
    · simp [h_deadline] at h_dl
    · simp [h_available] at h_av
    · omega
  have h_not_stale : ¬ pluginJobStale (pluginJobRecover row) := by
    rw [h_first]
    rintro ⟨-, h_reason⟩
    rcases h_reason with h_lt | h_dl | h_av | ⟨-, -, h_quiet⟩
    · exact Nat.lt_irrefl _ h_lt
    · simp [h_deadline] at h_dl
    · simp [h_available] at h_av
    · rcases h_quiet with h_unstepped | h_none
      · simp at h_unstepped
      · simp at h_none; omega
  rw [pluginJobRow_not_stale_unchanged _ h_not_stale, h_first]
  exact h_running

/-- A tick re-drives one step, then
    the stall rule fails the job closed when that step left it stalled. -/
def pluginJobRecoverySweep : RecoverySweep :=
  { Row := PluginJobRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_recover_plugin_job_rows"
  , rustFunction := "plugin_job::recovery::redrive_running_jobs"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := pluginJobStale
  , recover := fun row => pluginJobRecover (pluginJobRecover row)
  , terminal := fun row => ¬ pluginJobStale row
  , measure := fun row => if pluginJobStale row then 1 else 0
  , h_stale_positive := by
      intro row h
      simp [h]
  , h_recover_terminal := pluginJobRow_tick_not_stale
  , h_recover_zero := by
      intro row h
      simp [pluginJobRow_tick_not_stale row h]
  }

end Recovery
