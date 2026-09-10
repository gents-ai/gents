import Proofs.Triggers.Types
import Proofs.Configuration

def dispatchEnabledForSchedule
    (snap : TriggerSnapshot) (triggerId : String) : Option ActiveSchedule :=
  snap.activeSchedules.find? (fun s => (s.triggerId == triggerId) && s.enabled)

def dispatchEnabledForEvent
    (snap : TriggerSnapshot) (triggerId : String) : Option ActiveEventTrigger :=
  snap.activeEventTriggers.find? (fun t => (t.triggerId == triggerId) && t.enabled)

def dispatch
    (snap : TriggerSnapshot) (intent : FireIntent) : Option RequestSeed :=
  match intent.triggerKind with
  | .schedule =>
    match intent.triggerId with
    | none     => none
    | some tid =>
      match dispatchEnabledForSchedule snap tid with
      | none   => none
      | some active =>
        some { taskId := active.taskId, causedByTriggerId := some tid, causedByTriggerKind := .schedule }
  | .event =>
    match intent.triggerId with
    | none     => none
    | some tid =>
      match dispatchEnabledForEvent snap tid with
      | none   => none
      | some active =>
        some { taskId := active.taskId, causedByTriggerId := some tid, causedByTriggerKind := .event }
  | .manual =>
    some { taskId := intent.taskId, causedByTriggerId := none, causedByTriggerKind := .manual }

theorem dispatch_manual_lineage_id_is_none
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) :
    dispatch snap intent = some seed →
    seed.causedByTriggerKind = .manual →
    seed.causedByTriggerId = none := by
  intro h_dispatch h_manual
  unfold dispatch at h_dispatch
  cases h_kind : intent.triggerKind with
  | schedule =>
    rw [h_kind] at h_dispatch
    match h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForSchedule snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        obtain ⟨⟩ := h_dispatch
        simp at h_manual
  | event =>
    rw [h_kind] at h_dispatch
    match h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForEvent snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        obtain ⟨⟩ := h_dispatch
        simp at h_manual
  | manual =>
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    obtain ⟨⟩ := h_dispatch
    rfl

private theorem find?_some_and_mem
    {α : Type} {p : α → Bool} {l : List α} {a : α}
    (h : l.find? p = some a) : a ∈ l ∧ p a = true := by
  induction l with
  | nil => simp [List.find?] at h
  | cons x xs ih =>
    simp only [List.find?] at h
    split at h
    ·
      rename_i h_pred
      cases h
      exact ⟨List.mem_cons_self _ _, h_pred⟩
    ·
      have := ih h
      exact ⟨List.mem_cons_of_mem _ this.1, this.2⟩

theorem T1_enabled_gate
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) :
    dispatch snap intent = some seed →
    (intent.triggerKind = .schedule →
      ∃ triggerId, intent.triggerId = some triggerId ∧
        ∃ sched ∈ snap.activeSchedules,
          sched.triggerId = triggerId ∧ sched.enabled = true) ∧
    (intent.triggerKind = .event →
      ∃ triggerId, intent.triggerId = some triggerId ∧
        ∃ trig ∈ snap.activeEventTriggers,
          trig.triggerId = triggerId ∧ trig.enabled = true) := by
  intro h_dispatch
  refine ⟨?schedule, ?event⟩
  ·
    intro h_kind
    unfold dispatch at h_dispatch
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForSchedule snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some active =>
        unfold dispatchEnabledForSchedule at h_found
        have ⟨h_mem, h_pred⟩ := find?_some_and_mem h_found
        have ⟨h_beq, h_enabled⟩ := (Bool.and_eq_true _ _).mp h_pred
        refine ⟨tid, rfl, active, h_mem, ?_, ?_⟩
        · exact beq_iff_eq.mp h_beq
        · exact h_enabled
  ·
    intro h_kind
    unfold dispatch at h_dispatch
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForEvent snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some active =>
        unfold dispatchEnabledForEvent at h_found
        have ⟨h_mem, h_pred⟩ := find?_some_and_mem h_found
        have ⟨h_beq, h_enabled⟩ := (Bool.and_eq_true _ _).mp h_pred
        refine ⟨tid, rfl, active, h_mem, ?_, ?_⟩
        · exact beq_iff_eq.mp h_beq
        · exact h_enabled

theorem T1_manual_unconditional
    (snap : TriggerSnapshot) (intent : FireIntent) :
    intent.triggerKind = .manual →
    dispatch snap intent =
      some { taskId := intent.taskId, causedByTriggerId := none, causedByTriggerKind := .manual } := by
  intro h_kind
  unfold dispatch
  rw [h_kind]

/-- A missing/disabled trigger produces no work. A selected task goes through the
same owned resolver as direct task and graph execution; resolution errors remain
visible to the caller. `scope` is the principal owning this runtime snapshot. -/
def resolveDispatch (registry : Configuration.Registry) (scope : String)
    (snap : TriggerSnapshot) (intent : FireIntent) :
    Option (Except Configuration.ResolveError Configuration.ResolvedSessionConfig) :=
  (dispatch snap intent).map fun seed =>
    Configuration.resolveTask registry scope seed.taskId

theorem dispatch_uses_common_task_resolver (registry : Configuration.Registry)
    (scope : String) (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed)
    (h : dispatch snap intent = some seed) :
    resolveDispatch registry scope snap intent =
      some (Configuration.resolveTask registry scope seed.taskId) := by
  simp [resolveDispatch, h]

theorem scheduled_dispatch_selects_configured_task (snap : TriggerSnapshot)
    (intent : FireIntent) (tid : String) (active : ActiveSchedule)
    (hk : intent.triggerKind = .schedule) (hi : intent.triggerId = some tid)
    (ha : dispatchEnabledForSchedule snap tid = some active) :
    (dispatch snap intent).map RequestSeed.taskId = some active.taskId := by
  simp [dispatch, hk, hi, ha]

theorem event_dispatch_selects_configured_task (snap : TriggerSnapshot)
    (intent : FireIntent) (tid : String) (active : ActiveEventTrigger)
    (hk : intent.triggerKind = .event) (hi : intent.triggerId = some tid)
    (ha : dispatchEnabledForEvent snap tid = some active) :
    (dispatch snap intent).map RequestSeed.taskId = some active.taskId := by
  simp [dispatch, hk, hi, ha]

/-- Successful trigger execution has an owned, enabled Task and delegates its
behavior to the canonical resolver. Trigger lineage cannot bypass task admission. -/
theorem resolveDispatch_ok_iff (registry : Configuration.Registry)
    (scope : String) (snap : TriggerSnapshot) (intent : FireIntent)
    (session : Configuration.ResolvedSessionConfig) :
    resolveDispatch registry scope snap intent = some (.ok session) ↔
      ∃ seed, dispatch snap intent = some seed ∧
        Configuration.resolveTask registry scope seed.taskId = .ok session := by
  cases h : dispatch snap intent with
  | none => simp [resolveDispatch, h]
  | some seed => simp [resolveDispatch, h]
