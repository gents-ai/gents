import Proofs.Triggers.Dispatch

def dispatchStep
    (state  : SystemState)
    (snap   : TriggerSnapshot)
    (intent : FireIntent)
    : SystemState :=
  match dispatch snap intent with
  | none => state
  | some seed =>
    let key : Option TriggerKey :=
      match seed.causedByTriggerId with
      | none     => none
      | some tid => some (tid, seed.causedByTriggerKind)
    let origin : ExecutionOrigin :=
      match seed.causedByTriggerKind with
      | .manual            => .interactive
      | .schedule | .event => .scheduled
    let newId : String := s!"dispatched-{state.requests.length}"
    let newRequest : AgentRequest :=
      { id := newId
      , causedBy := key
      , concurrency := intent.concurrency
      , isTerminal := false
      , executionOrigin := origin }
    match intent.concurrency with
    | .parallel =>
      { state with requests := state.requests ++ [newRequest] }
    | .serial =>
      match key with
      | none =>
        { state with requests := state.requests ++ [newRequest] }
      | some t =>
        if state.requests.any (fun r => (r.causedBy == some t) && !r.isTerminal) then
          state
        else
          { state with requests := state.requests ++ [newRequest] }
    | .latestOnly =>
      match key with
      | none =>
        { state with requests := state.requests ++ [newRequest] }
      | some t =>
        let superseded := state.requests.map (fun r =>
          if (r.causedBy == some t) && !r.isTerminal then
            { r with isTerminal := true }
          else r)
        { state with requests := superseded ++ [newRequest] }

def lifecycleTerminateStep (s : SystemState) (reqId : String) : SystemState :=
  { s with requests := s.requests.map (fun r =>
      if (r.id == reqId) && !r.isTerminal then
        { r with isTerminal := true }
      else r) }

inductive Reachable : SystemState → Prop where
  | empty : Reachable SystemState.empty
  | step (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) :
      Reachable s → Reachable (dispatchStep s snap intent)
  | terminate (s : SystemState) (reqId : String) :
      Reachable s → Reachable (lifecycleTerminateStep s reqId)

inductive ReachableUnder (P : FireIntent → Prop) : SystemState → Prop where
  | empty : ReachableUnder P SystemState.empty
  | step (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) :
      P intent →
      ReachableUnder P s →
      ReachableUnder P (dispatchStep s snap intent)
  | terminate (s : SystemState) (reqId : String) :
      ReachableUnder P s →
      ReachableUnder P (lifecycleTerminateStep s reqId)

abbrev WellFormedReachable : SystemState → Prop :=
  ReachableUnder FireIntent.WellFormed

abbrev SeriallyReachable (t : TriggerKey) : SystemState → Prop :=
  ReachableUnder (fun intent => intent.WellFormed ∧ intent.SerialForKey t)

theorem ReachableUnder.toReachable
    {P : FireIntent → Prop} {s : SystemState} :
    ReachableUnder P s → Reachable s := by
  intro h_reach
  induction h_reach with
  | empty =>
      exact Reachable.empty
  | step s snap intent _ h_prev ih =>
      exact Reachable.step s snap intent ih
  | terminate s reqId h_prev ih =>
      exact Reachable.terminate s reqId ih

theorem nonTerminalCountFor_empty (t : TriggerKey) :
    SystemState.empty.nonTerminalCountFor t = 0 := by
  simp [SystemState.empty, SystemState.nonTerminalCountFor]

private theorem dispatchStep_preserves_causedBy_and_concurrency
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent)
    (r : AgentRequest) :
    r ∈ s.requests →
    ∃ r' ∈ (dispatchStep s snap intent).requests,
      r'.causedBy = r.causedBy ∧ r'.concurrency = r.concurrency := by
  intro h_mem
  unfold dispatchStep
  cases h_disp : dispatch snap intent with
  | none =>
    exact ⟨r, h_mem, rfl, rfl⟩
  | some seed =>
    simp only
    cases h_conc : intent.concurrency with
    | parallel =>
      simp only
      refine ⟨r, ?_, rfl, rfl⟩
      exact List.mem_append_left _ h_mem
    | serial =>
      simp only
      cases h_key : seed.causedByTriggerId with
      | none =>
        simp only
        refine ⟨r, ?_, rfl, rfl⟩
        exact List.mem_append_left _ h_mem
      | some tid =>
        simp only
        by_cases h_any :
          s.requests.any (fun r => (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal) = true
        · rw [if_pos h_any]
          exact ⟨r, h_mem, rfl, rfl⟩
        · rw [if_neg h_any]
          refine ⟨r, ?_, rfl, rfl⟩
          exact List.mem_append_left _ h_mem
    | latestOnly =>
      simp only
      cases h_key : seed.causedByTriggerId with
      | none =>
        simp only
        refine ⟨r, ?_, rfl, rfl⟩
        exact List.mem_append_left _ h_mem
      | some tid =>
        simp only
        refine ⟨
          if (r.causedBy == some (tid, seed.causedByTriggerKind)) && !r.isTerminal then
            { r with isTerminal := true }
          else r,
          ?_, ?_, ?_⟩
        ·
          apply List.mem_append_left
          exact List.mem_map_of_mem _ h_mem
        ·
          split <;> rfl
        ·
          split <;> rfl

private theorem dispatch_seed_some_triggerId_matches_intent
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed)
    {tid : String}
    (h_dispatch : dispatch snap intent = some seed)
    (h_seedId : seed.causedByTriggerId = some tid) :
    intent.triggerId = some tid ∧ intent.triggerKind = seed.causedByTriggerKind := by
  unfold dispatch at h_dispatch
  cases h_kind : intent.triggerKind with
  | schedule =>
    rw [h_kind] at h_dispatch
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some intentTid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForSchedule snap intentTid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        injection h_dispatch with h_seed_eq
        subst h_seed_eq
        simp at h_seedId
        obtain rfl := h_seedId
        exact ⟨rfl, rfl⟩
  | event =>
    rw [h_kind] at h_dispatch
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some intentTid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      cases h_found : dispatchEnabledForEvent snap intentTid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some _ =>
        rw [h_found] at h_dispatch
        simp only at h_dispatch
        injection h_dispatch with h_seed_eq
        subst h_seed_eq
        simp at h_seedId
        obtain rfl := h_seedId
        exact ⟨rfl, rfl⟩
  | manual =>
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    injection h_dispatch with h_seed_eq
    subst h_seed_eq
    simp at h_seedId

theorem dispatch_key_matches_intent_target
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) (t : TriggerKey)
    (h_dispatch : dispatch snap intent = some seed)
    (h_key :
      (match seed.causedByTriggerId with
      | none => none
      | some tid => some (tid, seed.causedByTriggerKind)) = some t) :
    intent.triggerId = some t.1 ∧ intent.triggerKind = t.2 := by
  cases h_seedId : seed.causedByTriggerId with
  | none =>
    simp [h_seedId] at h_key
  | some tid =>
    have h_tuple : (tid, seed.causedByTriggerKind) = t := by
      simpa [h_seedId] using h_key
    have ⟨h_triggerId, h_kind⟩ :=
      dispatch_seed_some_triggerId_matches_intent snap intent seed h_dispatch h_seedId
    cases t with
    | mk tid' kind' =>
      cases h_tuple
      simpa using And.intro h_triggerId h_kind

theorem dispatchStep_hypothesis_preservation
    (s : SystemState) (snap : TriggerSnapshot) (intent : FireIntent) (t : TriggerKey)
    (h_hyp_post : ∀ r ∈ (dispatchStep s snap intent).requests,
                  r.causedBy = some t → r.concurrency = .serial) :
    ∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial := by
  intro r h_mem h_causedBy
  obtain ⟨r', h_mem', h_cb, h_conc⟩ :=
    dispatchStep_preserves_causedBy_and_concurrency s snap intent r h_mem
  have h_causedBy' : r'.causedBy = some t := h_cb.trans h_causedBy
  have h_serial' := h_hyp_post r' h_mem' h_causedBy'
  exact h_conc ▸ h_serial'

theorem map_member_has_preimage_preserving_causedBy_and_concurrency
    (s : SystemState)
    (f : AgentRequest → AgentRequest)
    (h_preserve : ∀ r, (f r).causedBy = r.causedBy ∧ (f r).concurrency = r.concurrency)
    (r : AgentRequest) :
    r ∈ s.requests.map f →
    ∃ r0 ∈ s.requests, r.causedBy = r0.causedBy ∧ r.concurrency = r0.concurrency := by
  intro h_mem
  rcases List.mem_map.mp h_mem with ⟨r0, h_mem0, h_eq⟩
  refine ⟨r0, h_mem0, ?_, ?_⟩
  · cases h_eq
    exact (h_preserve r0).1
  · cases h_eq
    exact (h_preserve r0).2

theorem list_filter_map_length_le_filter_length
    {α : Type} {p : α → Bool} (f : α → α) (l : List α)
    (h_mono : ∀ a, p (f a) = true → p a = true) :
    ((l.map f).filter p).length ≤ (l.filter p).length := by
  induction l with
  | nil => simp
  | cons hd tl ih =>
    simp only [List.map_cons, List.filter_cons]
    by_cases h_p_f_hd : p (f hd) = true
    ·
      have h_p_hd : p hd = true := h_mono hd h_p_f_hd
      rw [if_pos h_p_f_hd, if_pos h_p_hd]
      simp only [List.length_cons]
      exact Nat.succ_le_succ ih
    ·
      have h_p_f_hd_false : p (f hd) = false := by
        cases h : p (f hd) with
        | false => rfl
        | true => exact absurd h h_p_f_hd
      rw [if_neg (by rw [h_p_f_hd_false]; decide)]
      by_cases h_p_hd : p hd = true
      · rw [if_pos h_p_hd]
        simp only [List.length_cons]
        exact Nat.le_succ_of_le ih
      · have h_p_hd_false : p hd = false := by
          cases h : p hd with
          | false => rfl
          | true => exact absurd h h_p_hd
        rw [if_neg (by rw [h_p_hd_false]; decide)]
        exact ih

theorem lifecycleTerminateStep_preserves_bound
    (s : SystemState) (reqId : String) (t : TriggerKey) :
    (lifecycleTerminateStep s reqId).nonTerminalCountFor t
      ≤ s.nonTerminalCountFor t := by
  simp only [SystemState.nonTerminalCountFor, lifecycleTerminateStep]
  apply list_filter_map_length_le_filter_length
  intro r h_p_f_r
  cases h_cond : (r.id == reqId) && !r.isTerminal with
  | true =>
    rw [if_pos h_cond] at h_p_f_r
    simp at h_p_f_r
  | false =>
    rw [if_neg (by rw [h_cond]; decide)] at h_p_f_r
    exact h_p_f_r

theorem lifecycleTerminateStep_preserves_causedBy_and_concurrency
    (s : SystemState) (reqId : String) (r : AgentRequest) :
    r ∈ s.requests →
    ∃ r' ∈ (lifecycleTerminateStep s reqId).requests,
      r'.causedBy = r.causedBy ∧ r'.concurrency = r.concurrency := by
  intro h_mem
  unfold lifecycleTerminateStep
  simp only
  refine ⟨_, List.mem_map_of_mem _ h_mem, ?_, ?_⟩
  · split <;> rfl
  · split <;> rfl
