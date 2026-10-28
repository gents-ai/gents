import Proofs.Session.Properties.Coalescing

namespace SessionQueue

inductive AgentMessageMode where
  | queue
  | steer
  | interrupt
  deriving DecidableEq, Repr

/-- An interrupt requires separate authority and is requested only after the
message is durably admitted. The delivery marker alone does not grant a lease or
allow input to pass an earlier queue entry. -/
def agentMessageDelivery? (mode : AgentMessageMode) (mayInterrupt : Bool) :
    Option (QueueDelivery × Bool) :=
  match mode with
  | .queue => some (.queue, false)
  | .steer => some (.steer, false)
  | .interrupt => if mayInterrupt then some (.queue, true) else none

theorem agent_message_interrupt_requires_authority :
    agentMessageDelivery? .interrupt false = none := by
  rfl

theorem agent_message_interrupt_is_queued :
    agentMessageDelivery? .interrupt true = some (.queue, true) := by
  rfl

theorem agent_message_steer_does_not_interrupt (allowed : Bool) :
    agentMessageDelivery? .steer allowed = some (.steer, false) := by
  rfl

def steersInto (active candidate : QueueEntry) : Prop :=
  candidate.fresh = true ∧ candidate.delivery = .steer ∧ candidate.policy = .append ∧
  (candidate.source = .user ∨ candidate.source = .steering) ∧
  candidate.origin = .interactive ∧ candidate.requester = active.requester ∧
  candidate.turnContext = active.turnContext ∧
  candidate.goalAssignment = active.goalAssignment

instance (active candidate : QueueEntry) : Decidable (steersInto active candidate) := by
  unfold steersInto
  infer_instance

def steeringRun (active : QueueEntry) (admitted : List RequestId) :
    List QueueEntry → List QueueEntry
  | [] => []
  | entry :: rest =>
      if steersInto active entry ∧ entry.requestId ∈ admitted then
        entry :: steeringRun active admitted rest
      else []

/-- The native execution owner supplies the exact active row and generation;
this operation grants no lease. -/
def intakeSteering? (target : AgentSession.Scope) (pre : SessionQueueState)
    (active : QueueEntry) (admitted : List RequestId) (safeBoundary : Bool) :
    Option SessionQueueState :=
  if pre.scope = target ∧ pre.active = some active.requestId ∧
      pre.folding = [] ∧ safeBoundary then
    let selected := steeringRun active admitted pre.pending
    some { pre with folding := selected, pending := pre.pending.drop selected.length }
  else none

def replacementDeliveryAllowed (before after : QueueDelivery) : Prop :=
  before = after ∨ (before = .queue ∧ after = .steer)

instance (before after : QueueDelivery) : Decidable (replacementDeliveryAllowed before after) := by
  unfold replacementDeliveryAllowed
  infer_instance

theorem steering_delivery_cannot_be_demoted :
    ¬ replacementDeliveryAllowed .steer .queue := by
  simp [replacementDeliveryAllowed]

theorem queued_delivery_can_be_promoted :
    replacementDeliveryAllowed .queue .steer := by
  exact Or.inr ⟨rfl, rfl⟩

/-- The active request and already-consumed input cannot be edited; selection is
provisional until authored consumption (an edit invalidates it and restores the
remaining order). -/
def replacementAllowed (target : AgentSession.Scope) (pre : SessionQueueState)
    (caller : Option Nat) (expected : List RequestId) (offset count : Nat)
    (replacements : List QueueEntry) : Bool :=
  let pending := pre.folding ++ pre.pending
  let old := (pending.drop offset).take count
  let fresh := replacements.map (·.requestId)
  decide (pre.scope = target) && decide (pending.map (·.requestId) = expected) &&
      decide (count > 0) && decide (old.length = count) &&
      old.all (fun entry => decide (entry.source = .user ∧ entry.policy = .append ∧
        entry.origin = .interactive ∧ entry.requester = caller ∧ entry.fresh = true)) &&
      (match old.head? with
        | none => true
        | some first => old.all (fun entry => decide (entry.turnContext = first.turnContext))) &&
      decide (replacements = [] ∨ replacements.length = count) && decide fresh.Nodup &&
      replacements.all (fun entry => decide (RequestIdFresh pre entry)) &&
      (old.zip replacements).all (fun pair => decide (
        pair.2.source = .user ∧ pair.2.policy = .append ∧
        pair.2.origin = .interactive ∧ pair.2.requester = caller ∧
        pair.2.turnContext = pair.1.turnContext ∧
        replacementDeliveryAllowed pair.1.delivery pair.2.delivery ∧
        pair.2.orderKey = pair.1.orderKey))

def replacePendingGroup? (target : AgentSession.Scope) (pre : SessionQueueState)
    (caller : Option Nat) (expected : List RequestId) (offset count : Nat)
    (replacements : List QueueEntry) : Option SessionQueueState :=
  let pending := pre.folding ++ pre.pending
  let old := (pending.drop offset).take count
  if replacementAllowed target pre caller expected offset count replacements then
    some { pre with
      pending := pending.take offset ++ replacements ++ pending.drop (offset + count)
      folding := []
      terminal := pre.terminal ∪ (old.map (·.requestId)).toFinset }
  else none

theorem intake_preserves_active {target : AgentSession.Scope} {pre post : SessionQueueState}
    {active : QueueEntry} {admitted : List RequestId} {safe : Bool}
    (h : intakeSteering? target pre active admitted safe = some post) :
    post.active = pre.active ∧ post.terminal = pre.terminal := by
  unfold intakeSteering? at h
  split at h
  · cases h
    exact ⟨rfl, rfl⟩
  · contradiction

theorem unsafe_boundary_refuses (target : AgentSession.Scope) (pre : SessionQueueState)
    (active : QueueEntry) (admitted : List RequestId) :
    intakeSteering? target pre active admitted false = none := by
  simp [intakeSteering?]

theorem queue_delivery_is_barrier (active entry : QueueEntry) (rest : List QueueEntry)
    (admitted : List RequestId) (h : entry.delivery = .queue) :
    steeringRun active admitted (entry :: rest) = [] := by
  simp [steeringRun, steersInto, h]

theorem stale_input_is_barrier (active entry : QueueEntry) (rest : List QueueEntry)
    (admitted : List RequestId) (h : entry.fresh = false) :
    steeringRun active admitted (entry :: rest) = [] := by
  simp [steeringRun, steersInto, h]

theorem different_assignment_is_barrier (active entry : QueueEntry) (rest : List QueueEntry)
    (admitted : List RequestId) (different : entry.goalAssignment ≠ active.goalAssignment) :
    steeringRun active admitted (entry :: rest) = [] := by
  simp [steeringRun, steersInto, different]

theorem replacement_preserves_active_and_invalidates_selection
    {target : AgentSession.Scope} {pre post : SessionQueueState}
    {caller : Option Nat} {expected : List RequestId} {offset count : Nat}
    {replacements : List QueueEntry}
    (h : replacePendingGroup? target pre caller expected offset count replacements = some post) :
    post.active = pre.active ∧ post.folding = [] := by
  unfold replacePendingGroup? at h
  simp only at h
  split at h
  · cases h
    exact ⟨rfl, rfl⟩
  · contradiction

theorem steeringRun_append_drop (active : QueueEntry) (admitted : List RequestId)
    (entries : List QueueEntry) :
    steeringRun active admitted entries ++
      entries.drop (steeringRun active admitted entries).length = entries := by
  induction entries with
  | nil => rfl
  | cons entry rest ih =>
      by_cases h : steersInto active entry ∧ entry.requestId ∈ admitted
      · simp only [steeringRun, h, and_self, ↓reduceIte, List.length_cons, List.drop_succ_cons,
          List.cons_append, ih]
      · simp [steeringRun, h]

theorem intake_preserves_queue {target : AgentSession.Scope} {pre post : SessionQueueState}
    {active : QueueEntry} {admitted : List RequestId} {safe : Bool}
    (h : intakeSteering? target pre active admitted safe = some post) :
    post.folding ++ post.pending = pre.folding ++ pre.pending ∧
      post.active = pre.active ∧ post.terminal = pre.terminal := by
  unfold intakeSteering? at h
  split at h
  · rename_i hc
    cases h
    refine ⟨?_, rfl, rfl⟩
    rw [hc.2.2.1]
    exact steeringRun_append_drop active admitted pre.pending
  · contradiction

private theorem orderKeys_of_zip {old fresh : List QueueEntry}
    (hl : old.length = fresh.length)
    (h : ∀ p ∈ old.zip fresh, p.2.orderKey = p.1.orderKey) :
    fresh.map (·.orderKey) = old.map (·.orderKey) := by
  induction old generalizing fresh with
  | nil => cases fresh <;> simp_all
  | cons o os ih =>
      cases fresh with
      | nil => simp at hl
      | cons f fs =>
          simp only [List.zip_cons_cons, List.mem_cons, forall_eq_or_imp] at h
          simp only [List.map_cons, h.1, ih (by simpa using hl) h.2]

private theorem mem_zip_right {old fresh : List QueueEntry}
    (hl : old.length = fresh.length) {y : QueueEntry} (hy : y ∈ fresh) :
    ∃ x, (x, y) ∈ old.zip fresh := by
  induction old generalizing fresh with
  | nil => cases fresh <;> simp_all
  | cons o os ih =>
      cases fresh with
      | nil => simp at hy
      | cons f fs =>
          simp only [List.mem_cons] at hy
          rcases hy with rfl | hy
          · exact ⟨o, by simp⟩
          · obtain ⟨x, hx⟩ := ih (by simpa using hl) hy
            exact ⟨x, by simp [hx]⟩

theorem replacementAllowed_facts {target : AgentSession.Scope} {pre : SessionQueueState}
    {caller : Option Nat} {expected : List RequestId} {offset count : Nat}
    {replacements : List QueueEntry}
    (h : replacementAllowed target pre caller expected offset count replacements = true) :
    (((pre.folding ++ pre.pending).drop offset).take count).length = count ∧
      (replacements = [] ∨ replacements.length = count) ∧
      (replacements.map (·.requestId)).Nodup ∧
      (∀ r ∈ replacements, RequestIdFresh pre r ∧ r.policy = .append) ∧
      (∀ p ∈ (((pre.folding ++ pre.pending).drop offset).take count).zip replacements,
        p.2.orderKey = p.1.orderKey) := by
  simp only [replacementAllowed, Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true] at h
  obtain ⟨⟨⟨⟨⟨⟨⟨⟨⟨_, _⟩, _⟩, hlen⟩, _⟩, _⟩, hcount⟩, hnodup⟩, hfresh⟩, hzip⟩ := h
  refine ⟨hlen, hcount, hnodup, fun r hr => ⟨hfresh r hr, ?_⟩,
    fun p hp => (hzip p hp).2.2.2.2.2.2⟩
  rcases hcount with rfl | hcount
  · simp at hr
  · obtain ⟨x, hx⟩ := mem_zip_right (hlen.trans hcount.symm) hr
    exact (hzip _ hx).2.1

theorem take_group_drop (pending : List QueueEntry) (offset count : Nat) :
    pending.take offset ++ ((pending.drop offset).take count ++ pending.drop (offset + count)) =
      pending := by
  rw [← List.drop_drop, List.take_append_drop, List.take_append_drop]

private theorem uniqueCoalesced_insert_append {before after inserted : List QueueEntry}
    (h_unique : UniqueCoalescedQueueKeys (before ++ after))
    (h_append : ∀ r ∈ inserted, r.policy = .append) :
    UniqueCoalescedQueueKeys (before ++ (inserted ++ after)) := by
  induction before with
  | nil =>
      induction inserted with
      | nil => simpa using h_unique
      | cons r rs ih =>
          simp only [List.nil_append, List.cons_append, UniqueCoalescedQueueKeys]
          refine ⟨fun _ _ _ h_policy => ?_, ih (fun x hx => h_append x (by simp [hx]))⟩
          rw [h_append r (by simp)] at h_policy
          cases h_policy
  | cons b bs ih =>
      simp only [List.cons_append, UniqueCoalescedQueueKeys] at h_unique ⊢
      refine ⟨fun source key h_source h_policy h_key other h_other h_match => ?_, ih h_unique.2⟩
      simp only [List.mem_append] at h_other
      rcases h_other with h_other | h_other | h_other
      · exact h_unique.1 source key h_source h_policy h_key other (by simp [h_other]) h_match
      · have h_policy' := h_match.2.1
        rw [h_append other h_other] at h_policy'
        cases h_policy'
      · exact h_unique.1 source key h_source h_policy h_key other (by simp [h_other]) h_match

private theorem createdOrdered_of_orderKeys {left right : List QueueEntry}
    (h_keys : left.map (·.orderKey) = right.map (·.orderKey))
    (h_order : CreatedOrdered right) : CreatedOrdered left := by
  induction left generalizing right with
  | nil => trivial
  | cons l ls ih =>
      cases right with
      | nil => simp at h_keys
      | cons r rs =>
          simp only [List.map_cons, List.cons.injEq] at h_keys
          simp only [CreatedOrdered] at h_order ⊢
          refine ⟨fun other h_other => ?_, ih h_keys.2 h_order.2⟩
          have : other.orderKey ∈ rs.map (·.orderKey) := h_keys.2 ▸ List.mem_map.mpr ⟨other, h_other, rfl⟩
          obtain ⟨o, ho, hk⟩ := List.mem_map.mp this
          rw [h_keys.1, ← hk]
          exact h_order.1 o ho

theorem replace_shape {target : AgentSession.Scope} {pre post : SessionQueueState}
    {caller : Option Nat} {expected : List RequestId} {offset count : Nat}
    {replacements : List QueueEntry}
    (h : replacePendingGroup? target pre caller expected offset count replacements = some post) :
    replacementAllowed target pre caller expected offset count replacements = true ∧
      post.folding ++ post.pending = (pre.folding ++ pre.pending).take offset ++
        (replacements ++ (pre.folding ++ pre.pending).drop (offset + count)) ∧
      post.active = pre.active ∧
      post.terminal = pre.terminal ∪
        ((((pre.folding ++ pre.pending).drop offset).take count).map (·.requestId)).toFinset := by
  unfold replacePendingGroup? at h
  simp only at h
  split at h
  · rename_i hc
    cases h
    exact ⟨hc, by simp, rfl, rfl⟩
  · contradiction

theorem replace_admits_only_fresh_ids {target : AgentSession.Scope} {pre post : SessionQueueState}
    {caller : Option Nat} {expected : List RequestId} {offset count : Nat}
    {replacements : List QueueEntry}
    (h : replacePendingGroup? target pre caller expected offset count replacements = some post) :
    (replacements.map (·.requestId)).Nodup ∧ ∀ r ∈ replacements, RequestIdFresh pre r := by
  have facts := replacementAllowed_facts (replace_shape h).1
  exact ⟨facts.2.2.1, fun r hr => (facts.2.2.2.1 r hr).1⟩

theorem replace_preserves_createdOrdered {target : AgentSession.Scope}
    {pre post : SessionQueueState} {caller : Option Nat} {expected : List RequestId}
    {offset count : Nat} {replacements : List QueueEntry}
    (h : replacePendingGroup? target pre caller expected offset count replacements = some post)
    (h_order : CreatedOrdered (pre.folding ++ pre.pending)) :
    CreatedOrdered (post.folding ++ post.pending) := by
  obtain ⟨h_allowed, h_shape, -, -⟩ := replace_shape h
  obtain ⟨h_len, h_count, -, -, h_zip⟩ := replacementAllowed_facts h_allowed
  rw [h_shape]
  set pending := pre.folding ++ pre.pending
  rcases h_count with rfl | h_count
  · apply createdOrdered_sublist _ h_order
    conv => rhs; rw [← take_group_drop pending offset count]
    exact List.Sublist.append_left (List.sublist_append_right _ _) _
  · apply createdOrdered_of_orderKeys _ h_order
    conv => rhs; rw [← take_group_drop pending offset count]
    simp only [List.map_append, orderKeys_of_zip (h_len.trans h_count.symm) h_zip]

theorem replace_preserves_uniqueCoalescedQueueKeys {target : AgentSession.Scope}
    {pre post : SessionQueueState} {caller : Option Nat} {expected : List RequestId}
    {offset count : Nat} {replacements : List QueueEntry}
    (h : replacePendingGroup? target pre caller expected offset count replacements = some post)
    (h_unique : UniqueCoalescedQueueKeys (pre.folding ++ pre.pending)) :
    UniqueCoalescedQueueKeys (post.folding ++ post.pending) := by
  obtain ⟨h_allowed, h_shape, -, -⟩ := replace_shape h
  obtain ⟨-, -, -, h_fresh, -⟩ := replacementAllowed_facts h_allowed
  rw [h_shape]
  apply uniqueCoalesced_insert_append _ (fun r hr => (h_fresh r hr).2)
  apply uniqueCoalescedQueueKeys_sublist _ h_unique
  conv => rhs; rw [← take_group_drop (pre.folding ++ pre.pending) offset count]
  exact List.Sublist.append_left (List.sublist_append_right _ _) _

inductive ManagedStep : SessionQueueState → SessionQueueState → Prop where
  | queue {pre post : SessionQueueState} : Transition pre post → ManagedStep pre post
  | intake {target : AgentSession.Scope} {pre post : SessionQueueState} {active : QueueEntry}
      {admitted : List RequestId} {safe : Bool} :
      intakeSteering? target pre active admitted safe = some post → ManagedStep pre post
  | replace {target : AgentSession.Scope} {pre post : SessionQueueState} {caller : Option Nat}
      {expected : List RequestId} {offset count : Nat} {replacements : List QueueEntry} :
      replacePendingGroup? target pre caller expected offset count replacements = some post →
      ManagedStep pre post

abbrev ManagedTrace := Relation.ReflTransGen ManagedStep

theorem managed_trace_preserves_queue_invariants {pre post : SessionQueueState}
    (h_trace : ManagedTrace pre post)
    (h_order : CreatedOrdered (pre.folding ++ pre.pending))
    (h_unique : UniqueCoalescedQueueKeys (pre.folding ++ pre.pending)) :
    CreatedOrdered (post.folding ++ post.pending) ∧
      UniqueCoalescedQueueKeys (post.folding ++ post.pending) ∧
      pre.terminal ⊆ post.terminal := by
  induction h_trace with
  | refl => exact ⟨h_order, h_unique, subset_rfl⟩
  | tail _ h_step ih =>
      obtain ⟨h_order', h_unique', h_terminal⟩ := ih
      cases h_step with
      | queue h =>
          exact ⟨transition_preserves_createdOrdered h h_order',
            transition_preserves_uniqueCoalescedQueueKeys h h_unique',
            h_terminal.trans (terminal_history_monotonic h)⟩
      | intake h =>
          obtain ⟨h_queue, -, h_term⟩ := intake_preserves_queue h
          exact ⟨h_queue ▸ h_order', h_queue ▸ h_unique', h_term ▸ h_terminal⟩
      | replace h =>
          obtain ⟨-, -, -, h_term⟩ := replace_shape h
          exact ⟨replace_preserves_createdOrdered h h_order',
            replace_preserves_uniqueCoalescedQueueKeys h h_unique',
            h_term ▸ h_terminal.trans Finset.subset_union_left⟩

end SessionQueue
