import Proofs.Session.Management

namespace SessionQueue.InputEdit

/-- Facts come from existing signature, enrollment, applied-route and session
owners. A command without a peer route is local and requires requester=node;
remote commands require the exact fresh enrollment and applied peer route. -/
structure Authority where
  signatureValid : Bool
  replacementsValid : Bool
  sessionOwned : Bool
  requesterIsNode : Bool
  enrollmentFresh : Bool
  routeApplied : Bool
  /-- All physical command documents have been observed without relying on a unique
  index winner. Distinct documents sharing the command ID fail closed even for identical
  signed intents, and even when
  one already carries a valid receipt; committed effects are never undone. -/
  commandIdentityUnique : Bool := true
  deriving DecidableEq, Repr

structure Command where
  id : Nat
  digest : Nat
  caller : Option Nat
  hasPeer : Bool
  issuedAt : Nat
  expiresAt : Nat
  expected : List RequestId
  offset : Nat
  count : Nat
  replacements : List QueueEntry
  deriving DecidableEq, Repr

inductive Outcome where
  | applied
  | rejected
  deriving DecidableEq, Repr

structure Receipt where
  commandId : Nat
  digest : Nat
  outcome : Outcome
  requestIds : List RequestId
  deriving DecidableEq, Repr

structure State where
  queue : SessionQueueState
  receipts : List Receipt
  deriving DecidableEq

def authorized (a : Authority) (c : Command) (now : Nat) : Bool :=
  a.signatureValid && a.replacementsValid && a.sessionOwned && decide (now < c.expiresAt) &&
  if c.hasPeer then a.enrollmentFresh && a.routeApplied else a.requesterIsNode

/-- The queue holds the request in any position: active, selected, pending or terminal. -/
def recorded (q : SessionQueueState) (id : RequestId) : Prop :=
  q.active = some id ∨ id ∈ q.terminal ∨ id ∈ (q.folding ++ q.pending).map (·.requestId)

instance (q : SessionQueueState) (id : RequestId) : Decidable (recorded q id) := by
  unfold recorded
  infer_instance

/-- Replacement request IDs are presigned by the command author and admitted only
by this command, so their presence witnesses its committed effect even when its
receipt is missing or unverifiable. A cancellation has no such witness. -/
def effectsApplied (q : SessionQueueState) (c : Command) : Bool :=
  !c.replacements.isEmpty && c.replacements.all (fun e => decide (recorded q e.requestId))

def appliedReceipt (c : Command) : Receipt :=
  ⟨c.id, c.digest, .applied, c.replacements.map (·.requestId)⟩

def rejectedReceipt (c : Command) : Receipt := ⟨c.id, c.digest, .rejected, []⟩

/-- Replica clocks skew, so a command issued after the runtime's clock is left
undecided and reconsidered later; a future issuance must not become a permanent
signed rejection. Only reaching `expiresAt` rejects it. -/
def decideFresh (target : AgentSession.Scope) (now : Nat) (a : Authority)
    (c : Command) (s : State) : State × Option Receipt :=
  if effectsApplied s.queue c then
    ({ s with receipts := s.receipts ++ [appliedReceipt c] }, some (appliedReceipt c))
  else if now < c.issuedAt then (s, none)
  else
    match (if authorized a c now then
      replacePendingGroup? target s.queue c.caller c.expected c.offset c.count c.replacements
      else none) with
    | some queue => (⟨queue, s.receipts ++ [appliedReceipt c]⟩, some (appliedReceipt c))
    | none => ({ s with receipts := s.receipts ++ [rejectedReceipt c] }, some (rejectedReceipt c))

/-- Queue mutation and runtime-signed terminal receipt commit in one transaction.
The digest denotes the full signed intent. Exact replay returns the recorded
outcome without reapplying or reevaluating expiry; ID reuse with different
signed content is rejected without replacing the original receipt. -/
def apply (target : AgentSession.Scope) (now : Nat) (a : Authority)
    (c : Command) (s : State) : State × Option Receipt :=
  if !a.commandIdentityUnique then (s, none) else
  match s.receipts.find? (fun r => r.commandId == c.id) with
  | some receipt => if receipt.digest = c.digest then (s, some receipt) else (s, none)
  | none => decideFresh target now a c s

theorem decideFresh_cases (target : AgentSession.Scope) (now : Nat) (a : Authority)
    (c : Command) (s : State) :
    (effectsApplied s.queue c = true ∧ decideFresh target now a c s =
        ({ s with receipts := s.receipts ++ [appliedReceipt c] }, some (appliedReceipt c))) ∨
      decideFresh target now a c s = (s, none) ∨
      (∃ queue, authorized a c now = true ∧
        replacePendingGroup? target s.queue c.caller c.expected c.offset c.count
          c.replacements = some queue ∧
        decideFresh target now a c s =
          (⟨queue, s.receipts ++ [appliedReceipt c]⟩, some (appliedReceipt c))) ∨
      decideFresh target now a c s =
        ({ s with receipts := s.receipts ++ [rejectedReceipt c] }, some (rejectedReceipt c)) := by
  unfold decideFresh
  by_cases he : effectsApplied s.queue c
  · exact Or.inl ⟨he, by simp [he]⟩
  · by_cases hf : now < c.issuedAt
    · exact Or.inr (Or.inl (by simp [he, hf]))
    · by_cases ha : authorized a c now
      · cases hq : replacePendingGroup? target s.queue c.caller c.expected c.offset c.count
            c.replacements with
        | some queue => exact Or.inr (Or.inr (Or.inl ⟨queue, ha, rfl, by simp [he, hf, ha]⟩))
        | none => exact Or.inr (Or.inr (Or.inr (by simp [he, hf, ha, hq])))
      · exact Or.inr (Or.inr (Or.inr (by simp [he, hf, ha])))

theorem apply_cases (target : AgentSession.Scope) (now : Nat) (a : Authority)
    (c : Command) (s : State) :
    (apply target now a c s).1 = s ∨
      (s.receipts.find? (fun r => r.commandId == c.id) = none ∧
        apply target now a c s = decideFresh target now a c s) := by
  unfold apply
  by_cases hu : a.commandIdentityUnique
  · simp only [hu, Bool.not_true, Bool.false_eq_true, ↓reduceIte]
    split
    · split <;> exact Or.inl rfl
    · rename_i h_none
      exact Or.inr ⟨h_none, rfl⟩
  · simp [hu]

theorem recorded_replay_preserves_state (target : AgentSession.Scope) (now : Nat)
    (a : Authority) (c : Command) (s : State) (r : Receipt)
    (h : s.receipts.find? (fun r => r.commandId == c.id) = some r) :
    (apply target now a c s).1 = s := by
  by_cases hu : a.commandIdentityUnique <;>
    by_cases hd : r.digest = c.digest <;> simp [apply, hu, h, hd]

theorem observed_identity_collision_preserves_state (target : AgentSession.Scope)
    (now : Nat) (a : Authority) (c : Command) (s : State)
    (h : a.commandIdentityUnique = false) :
    apply target now a c s = (s, none) := by
  simp [apply, h]

theorem future_issuance_is_deferred (target : AgentSession.Scope) (now : Nat)
    (a : Authority) (c : Command) (s : State)
    (h_new : s.receipts.find? (fun r => r.commandId == c.id) = none)
    (h_effects : effectsApplied s.queue c = false) (h_future : now < c.issuedAt) :
    apply target now a c s = (s, none) := by
  by_cases hu : a.commandIdentityUnique <;>
    simp [apply, hu, h_new, decideFresh, h_effects, h_future]

/-- A rejected or unauthorized command leaves the queue exactly as it was. -/
theorem rejection_preserves_queue (target : AgentSession.Scope) (now : Nat)
    (a : Authority) (c : Command) (s : State)
    (h : (apply target now a c s).2.map (·.outcome) ≠ some .applied) :
    (apply target now a c s).1.queue = s.queue := by
  rcases apply_cases target now a c s with h_same | ⟨-, h_fresh⟩
  · rw [h_same]
  · rw [h_fresh] at h ⊢
    rcases decideFresh_cases target now a c s with ⟨-, hd⟩ | hd | ⟨q, -, -, hd⟩ | hd <;>
      simp_all [appliedReceipt]

theorem unauthorized_preserves_queue (target : AgentSession.Scope) (now : Nat)
    (a : Authority) (c : Command) (s : State) (h : authorized a c now = false) :
    (apply target now a c s).1.queue = s.queue := by
  rcases apply_cases target now a c s with h_same | ⟨-, h_fresh⟩
  · rw [h_same]
  · rw [h_fresh]
    rcases decideFresh_cases target now a c s with ⟨-, hd⟩ | hd | ⟨q, ha, -, -⟩ | hd
    · rw [hd]
    · rw [hd]
    · simp [h] at ha
    · rw [hd]

/-- At most one receipt per command ID is ever recorded. -/
def ReceiptsUnique (s : State) : Prop := (s.receipts.map (·.commandId)).Nodup

theorem apply_receipts (target : AgentSession.Scope) (now : Nat) (a : Authority)
    (c : Command) (s : State) :
    (apply target now a c s).1.receipts = s.receipts ∨
      (s.receipts.find? (fun r => r.commandId == c.id) = none ∧
        ∃ r : Receipt, r.commandId = c.id ∧
          (apply target now a c s).1.receipts = s.receipts ++ [r]) := by
  rcases apply_cases target now a c s with h_same | ⟨h_none, h_fresh⟩
  · exact Or.inl (by rw [h_same])
  · rw [h_fresh]
    rcases decideFresh_cases target now a c s with ⟨-, hd⟩ | hd | ⟨q, -, -, hd⟩ | hd <;>
      rw [hd]
    · exact Or.inr ⟨h_none, appliedReceipt c, rfl, rfl⟩
    · exact Or.inl rfl
    · exact Or.inr ⟨h_none, appliedReceipt c, rfl, rfl⟩
    · exact Or.inr ⟨h_none, rejectedReceipt c, rfl, rfl⟩

theorem apply_preserves_receipts_unique (target : AgentSession.Scope) (now : Nat)
    (a : Authority) (c : Command) (s : State) (h : ReceiptsUnique s) :
    ReceiptsUnique (apply target now a c s).1 := by
  unfold ReceiptsUnique at h ⊢
  rcases apply_receipts target now a c s with h_same | ⟨h_none, r, h_id, h_eq⟩
  · rwa [h_same]
  · rw [h_eq, List.map_append, List.nodup_append]
    refine ⟨h, List.nodup_singleton _, ?_⟩
    intro x hx hy
    simp only [List.map_cons, List.map_nil, List.mem_singleton] at hy
    obtain ⟨w, hw, rfl⟩ := List.mem_map.mp hx
    have := List.find?_eq_none.mp h_none w hw
    simp_all

/-- A recorded outcome is final: no later processing replaces or adds a receipt for it. -/
theorem recorded_receipt_is_final (target : AgentSession.Scope) (now : Nat)
    (a : Authority) (c : Command) (s : State) (id : Nat) (r : Receipt)
    (h : s.receipts.find? (fun r => r.commandId == id) = some r) :
    (apply target now a c s).1.receipts.find? (fun r => r.commandId == id) = some r := by
  rcases apply_receipts target now a c s with h_same | ⟨-, x, -, h_eq⟩
  · rwa [h_same]
  · rw [h_eq, List.find?_append, h]
    rfl

private theorem replace_records_replacements {target : AgentSession.Scope}
    {pre post : SessionQueueState} {caller : Option Nat} {expected : List RequestId}
    {offset count : Nat} {replacements : List QueueEntry}
    (h : replacePendingGroup? target pre caller expected offset count replacements = some post) :
    ∀ e ∈ replacements, recorded post e.requestId := by
  intro e he
  right; right
  rw [(replace_shape h).2.1]
  exact List.mem_map.mpr ⟨e, by simp [he], rfl⟩

private theorem recorded_of_entry {q : SessionQueueState} {x : QueueEntry}
    (h : x ∈ q.folding ∨ x ∈ q.pending ∨ x.requestId ∈ q.terminal ∨
      q.active = some x.requestId) : recorded q x.requestId := by
  rcases h with h | h | h | h
  · exact Or.inr (Or.inr (List.mem_map.mpr ⟨x, by simp [h], rfl⟩))
  · exact Or.inr (Or.inr (List.mem_map.mpr ⟨x, by simp [h], rfl⟩))
  · exact Or.inr (Or.inl h)
  · exact Or.inl h

private theorem drained_or_kept (source : QueueSource) (key : Option QueueKey)
    (allowed : QueueEntry → Bool) (entries : List QueueEntry) :
    ∀ x ∈ entries, x ∈ pendingAfterDrainMatching source key allowed entries ∨
      x.requestId ∈ drainedRequestIdsMatching source key allowed entries := by
  induction entries with
  | nil => simp
  | cons head tail ih =>
      intro x hx
      simp only [pendingAfterDrainMatching, drainedRequestIdsMatching]
      split
      · rcases List.mem_cons.mp hx with rfl | hx
        · exact Or.inr (Finset.mem_insert_self _ _)
        · rcases ih x hx with h | h
          · exact Or.inl h
          · exact Or.inr (Finset.mem_insert_of_mem h)
      · rcases List.mem_cons.mp hx with rfl | hx
        · exact Or.inl (List.mem_cons_self _ _)
        · rcases ih x hx with h | h
          · exact Or.inl (List.mem_cons_of_mem _ h)
          · exact Or.inr h

/-- An idle queue holds no selection: only an active claim selects input. -/
def IdleUnselected (q : SessionQueueState) : Prop := q.active = none → q.folding = []

theorem managed_step_preserves_idleUnselected {pre post : SessionQueueState}
    (h_step : ManagedStep pre post) (h : IdleUnselected pre) : IdleUnselected post := by
  unfold IdleUnselected at h ⊢
  cases h_step with
  | queue t =>
      cases t with
      | append_pending _ _ _ _ hp => subst hp; exact h
      | coalesce_pending_new _ _ _ _ hp => subst hp; exact h
      | coalesce_pending_existing _ _ hp => subst hp; exact h
      | claim_next _ _ hp => subst hp; simp [SessionQueueState.claimHead]
      | claim_folding _ _ hp => subst hp; simp [SessionQueueState.claimFolding]
      | consume_folded ha _ hp =>
          subst hp
          intro hn
          simp only [SessionQueueState.consumeFolded] at hn
          simp [hn] at ha
      | finish_active _ hp => subst hp; simp [SessionQueueState.finishActive]
      | drain_automated _ hp => subst hp; exact h
      | drain_observed_automated _ hp => subst hp; exact h
  | intake hi =>
      unfold intakeSteering? at hi
      split at hi
      · rename_i hc
        cases hi
        intro hn
        simp [hc.2.1] at hn
      · contradiction
  | replace hr =>
      unfold replacePendingGroup? at hr
      simp only at hr
      split at hr
      · cases hr
        intro
        rfl
      · contradiction

/-- A request the queue holds is never forgotten by any managed step. -/
theorem managed_step_preserves_recorded {pre post : SessionQueueState} {id : RequestId}
    (h_step : ManagedStep pre post) (h_idle : IdleUnselected pre) (h : recorded pre id) :
    recorded post id := by
  rcases h with h_active | h_terminal | h_queued
  · cases h_step with
    | queue t =>
        cases t with
        | append_pending _ _ _ _ hp => subst hp; exact Or.inl h_active
        | coalesce_pending_new _ _ _ _ hp => subst hp; exact Or.inl h_active
        | coalesce_pending_existing _ _ hp => subst hp; exact Or.inl h_active
        | claim_next ha _ _ => simp [ha] at h_active
        | claim_folding ha _ _ => simp [ha] at h_active
        | consume_folded _ _ hp => subst hp; exact Or.inl h_active
        | finish_active ha hp =>
            subst hp
            rw [ha] at h_active
            cases h_active
            exact Or.inr (Or.inl (Finset.mem_insert_self _ _))
        | drain_automated _ hp => subst hp; exact Or.inl h_active
        | drain_observed_automated _ hp => subst hp; exact Or.inl h_active
    | intake h => exact Or.inl ((intake_preserves_queue h).2.1 ▸ h_active)
    | replace h => exact Or.inl ((replace_shape h).2.2.1 ▸ h_active)
  · right; left
    cases h_step with
    | queue t => exact terminal_history_monotonic t h_terminal
    | intake h => exact (intake_preserves_queue h).2.2 ▸ h_terminal
    | replace h => exact (replace_shape h).2.2.2 ▸ Finset.mem_union_left _ h_terminal
  · obtain ⟨x, hx, rfl⟩ := List.mem_map.mp h_queued
    apply recorded_of_entry
    cases h_step with
    | queue t =>
        cases t with
        | append_pending _ _ _ _ hp =>
            subst hp
            simp only [List.mem_append] at hx
            simp [SessionQueueState.appendPending]
            tauto
        | coalesce_pending_new _ _ _ _ hp =>
            subst hp
            simp only [List.mem_append] at hx
            simp [SessionQueueState.appendPending]
            tauto
        | coalesce_pending_existing _ _ hp =>
            subst hp
            simp only [List.mem_append] at hx
            tauto
        | claim_next _ hpend hp =>
            subst hp
            rw [hpend] at hx
            simp only [List.mem_append, List.mem_cons] at hx
            simp only [SessionQueueState.claimHead]
            rcases hx with hx | rfl | hx
            · exact Or.inl hx
            · exact Or.inr (Or.inr (Or.inr rfl))
            · exact Or.inr (Or.inl hx)
        | @claim_folding entry rest admitted ha hpend hp =>
            subst hp
            rw [hpend, h_idle ha] at hx
            simp only [List.nil_append, List.mem_cons] at hx
            simp only [SessionQueueState.claimFolding]
            rcases hx with rfl | hx
            · exact Or.inr (Or.inr (Or.inr rfl))
            · rw [← foldRun_append_drop entry admitted rest, List.mem_append] at hx
              tauto
        | consume_folded _ hfold hp =>
            subst hp
            rw [hfold] at hx
            simp only [List.mem_append, List.mem_cons] at hx
            simp only [SessionQueueState.consumeFolded, Finset.mem_insert]
            tauto
        | finish_active _ hp =>
            subst hp
            simp only [List.mem_append] at hx
            simp only [SessionQueueState.finishActive, List.mem_append, List.nil_append]
            right; left; exact hx
        | @drain_automated source key _ hp =>
            subst hp
            simp only [List.mem_append] at hx
            simp only [SessionQueueState.drainAutomatedWakeups, pendingAfterDrain, drainedRequestIds,
              Finset.mem_union]
            rcases hx with hx | hx
            · exact Or.inl hx
            · rcases drained_or_kept source key (fun _ => true) pre.pending x hx with h | h
              · exact Or.inr (Or.inl h)
              · exact Or.inr (Or.inr (Or.inl (Or.inr h)))
        | @drain_observed_automated source key observed _ hp =>
            subst hp
            simp only [List.mem_append] at hx
            simp only [SessionQueueState.drainObservedAutomatedWakeups, Finset.mem_union]
            rcases hx with hx | hx
            · exact Or.inl hx
            · rcases drained_or_kept source key _ pre.pending x hx with h | h
              · exact Or.inr (Or.inl h)
              · exact Or.inr (Or.inr (Or.inl (Or.inr h)))
    | intake h =>
        have h_same := (intake_preserves_queue h).1
        have hx' : x ∈ _ := h_same ▸ hx
        simp only [List.mem_append] at hx'
        tauto
    | @replace _ _ _ _ _ offset count _ h =>
        obtain ⟨-, h_shape, -, h_term⟩ := replace_shape h
        have hx' := hx
        rw [← take_group_drop (pre.folding ++ pre.pending) offset count] at hx'
        simp only [List.mem_append] at hx'
        rcases hx' with hx' | hx' | hx'
        · have : x ∈ post.folding ++ post.pending := by rw [h_shape]; simp [hx']
          simp only [List.mem_append] at this
          tauto
        · right; right; left
          rw [h_term]
          exact Finset.mem_union_right _ (List.mem_toFinset.mpr (List.mem_map.mpr ⟨x, hx', rfl⟩))
        · have : x ∈ post.folding ++ post.pending := by rw [h_shape]; simp [hx']
          simp only [List.mem_append] at this
          tauto

theorem managed_trace_preserves_recorded {pre post : SessionQueueState} {id : RequestId}
    (h_trace : ManagedTrace pre post) (h_idle : IdleUnselected pre) (h : recorded pre id) :
    IdleUnselected post ∧ recorded post id := by
  induction h_trace with
  | refl => exact ⟨h_idle, h⟩
  | tail _ h_step ih =>
      exact ⟨managed_step_preserves_idleUnselected h_step ih.1,
        managed_step_preserves_recorded h_step ih.1 ih.2⟩

/-- Processing a command is either a no-op on the queue or one managed replacement step. -/
theorem apply_queue_step (target : AgentSession.Scope) (now : Nat) (a : Authority)
    (c : Command) (s : State) :
    (apply target now a c s).1.queue = s.queue ∨ ManagedStep s.queue (apply target now a c s).1.queue := by
  rcases apply_cases target now a c s with h_same | ⟨-, h_fresh⟩
  · exact Or.inl (by rw [h_same])
  · rw [h_fresh]
    rcases decideFresh_cases target now a c s with ⟨-, hd⟩ | hd | ⟨q, -, hq, hd⟩ | hd <;> rw [hd]
    · exact Or.inl rfl
    · exact Or.inl rfl
    · exact Or.inr (ManagedStep.replace hq)
    · exact Or.inl rfl

/-- Once a command's replacements are admitted, reprocessing it after any later
queue history, expiry or loss of its receipt yields the same applied receipt. -/
theorem once_applied_stays_applied (target target' : AgentSession.Scope) (now now' : Nat)
    (a a' : Authority) (c : Command) (s : State) (q : SessionQueueState)
    (receipts : List Receipt)
    (h : (apply target now a c s).2.map (·.outcome) = some .applied)
    (h_new : s.receipts.find? (fun r => r.commandId == c.id) = none)
    (h_nonempty : c.replacements ≠ [])
    (h_idle : IdleUnselected s.queue)
    (h_trace : ManagedTrace (apply target now a c s).1.queue q)
    (h_unique : a'.commandIdentityUnique = true)
    (h_unreceipted : receipts.find? (fun r => r.commandId == c.id) = none) :
    (apply target now a c s).2 = some (appliedReceipt c) ∧
      (apply target' now' a' c ⟨q, receipts⟩).2 = some (appliedReceipt c) := by
  have h_fresh : apply target now a c s = decideFresh target now a c s := by
    rcases apply_cases target now a c s with h_same | ⟨-, h_fresh⟩
    · by_cases hu : a.commandIdentityUnique
      · simp [apply, hu, h_new]
      · simp [apply, hu] at h
    · exact h_fresh
  have h_recorded : (apply target now a c s).2 = some (appliedReceipt c) ∧
      ∀ e ∈ c.replacements, recorded (apply target now a c s).1.queue e.requestId := by
    rw [h_fresh] at h ⊢
    rcases decideFresh_cases target now a c s with ⟨h_eff, hd⟩ | hd | ⟨queue, -, hq, hd⟩ | hd <;>
      rw [hd] at h ⊢
    · simp only [effectsApplied, Bool.and_eq_true, List.all_eq_true, decide_eq_true_eq] at h_eff
      exact ⟨rfl, h_eff.2⟩
    · simp at h
    · exact ⟨rfl, replace_records_replacements hq⟩
    · simp [rejectedReceipt] at h
  refine ⟨h_recorded.1, ?_⟩
  have h_idle' : IdleUnselected (apply target now a c s).1.queue := by
    rcases apply_queue_step target now a c s with h_same | h_step
    · rw [h_same]; exact h_idle
    · exact managed_step_preserves_idleUnselected h_step h_idle
  have h_effects : effectsApplied q c = true := by
    simp only [effectsApplied, Bool.and_eq_true, List.all_eq_true, decide_eq_true_eq,
      Bool.not_eq_true', List.isEmpty_eq_false_iff]
    exact ⟨h_nonempty, fun e he => (managed_trace_preserves_recorded h_trace h_idle' (h_recorded.2 e he)).2⟩
  simp [apply, h_unique, h_unreceipted, decideFresh, h_effects]

end SessionQueue.InputEdit
