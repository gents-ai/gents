import Proofs.Triggers.Types

/-!
# Trigger Dispatch

Enabled-gate dispatch semantics and T1 dispatch properties.
-/

/--
Helper: return the first enabled schedule in `snap.activeSchedules` matching
the given `triggerId`, or `none` if no such schedule exists.
-/
def dispatchEnabledForSchedule
    (snap : TriggerSnapshot) (triggerId : String) : Option ActiveSchedule :=
  snap.activeSchedules.find? (fun s => (s.triggerId == triggerId) && s.enabled)

/--
Helper: return the first enabled event trigger in `snap.activeEventTriggers`
matching the given `triggerId`, or `none` if no such trigger exists.
-/
def dispatchEnabledForEvent
    (snap : TriggerSnapshot) (triggerId : String) : Option ActiveEventTrigger :=
  snap.activeEventTriggers.find? (fun t => (t.triggerId == triggerId) && t.enabled)

/--
Dispatch — the enabled-gate + materialization-shape step.

Does NOT perform concurrency or in-flight checks against `SystemState`;
those live in `dispatchStep`.

Returns `some seed` when:
- Schedule kind: an enabled schedule matching `intent.triggerId` exists in the snapshot.
- Event kind: an enabled event trigger matching `intent.triggerId` exists.
- Manual kind: unconditional. Manual fires bypass the enabled gate; the
  task-level gate is enforced by the caller (`run_task_now` checks
  `snap.activeTasks` — out of scope for this proof layer).

Returns `none` otherwise.
-/
def dispatch
    (snap : TriggerSnapshot) (intent : FireIntent) : Option RequestSeed :=
  match intent.triggerKind with
  | .schedule =>
    match intent.triggerId with
    | none     => none
    | some tid =>
      match dispatchEnabledForSchedule snap tid with
      | none   => none
      | some _ =>
        some { causedByTriggerId := some tid, causedByTriggerKind := .schedule }
  | .event =>
    match intent.triggerId with
    | none     => none
    | some tid =>
      match dispatchEnabledForEvent snap tid with
      | none   => none
      | some _ =>
        some { causedByTriggerId := some tid, causedByTriggerKind := .event }
  | .manual =>
    -- Manual fires always carry a null lineage id, matching the runtime invariant
    -- (see `trigger_engine/manual_source.rs:83` where manual_source constructs
    -- `FireIntent { trigger_id: None, ... }`). Normalizing here rather than
    -- requiring a FireIntent well-formedness predicate keeps `Reachable`
    -- structurally simple; the caller's `intent.triggerId` (if any) is
    -- deliberately discarded. Also aligns with `consistentLineage` which
    -- maps `.manual → .interactive` without distinguishing an id.
    some { causedByTriggerId := none, causedByTriggerKind := .manual }

/--
Invariant: `dispatch` always produces a `RequestSeed` whose `causedByTriggerId`
is `none` for the `.manual` kind. Guaranteed by construction — the `.manual`
arm of `dispatch` hardcodes `causedByTriggerId := none`.

This rules out reachable manual requests with a non-null lineage id, matching
the runtime invariant in `trigger_engine/manual_source.rs`.
-/
theorem dispatch_manual_lineage_id_is_none
    (snap : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed) :
    dispatch snap intent = some seed →
    seed.causedByTriggerKind = .manual →
    seed.causedByTriggerId = none := by
  intro h_dispatch h_manual
  unfold dispatch at h_dispatch
  -- Case-split on triggerKind.
  cases h_kind : intent.triggerKind with
  | schedule =>
    rw [h_kind] at h_dispatch
    -- In the schedule arm, seed.causedByTriggerKind = .schedule, contradicting h_manual.
    -- Need to extract the kind field from the schedule-branch seed.
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
        -- h_dispatch : some { causedByTriggerId := some tid, causedByTriggerKind := .schedule } = some seed
        -- Extract seed.causedByTriggerKind:
        obtain ⟨⟩ := h_dispatch
        -- seed.causedByTriggerKind = .schedule, contradiction with h_manual
        simp at h_manual
  | event =>
    -- Symmetric to schedule.
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
    -- h_dispatch : some { causedByTriggerId := none, causedByTriggerKind := .manual } = some seed
    obtain ⟨⟩ := h_dispatch
    -- seed.causedByTriggerId = none directly
    rfl

/--
Local helper for T1: when `List.find? p l = some a`, both `a ∈ l` AND
`p a = true`. Mathlib provides these in two separate lemmas; we combine
them for proof convenience.
-/
private theorem find?_some_and_mem
    {α : Type} {p : α → Bool} {l : List α} {a : α}
    (h : l.find? p = some a) : a ∈ l ∧ p a = true := by
  induction l with
  | nil => simp [List.find?] at h
  | cons x xs ih =>
    simp only [List.find?] at h
    split at h
    · -- predicate true on x: h : some x = some a
      rename_i h_pred
      cases h
      exact ⟨List.mem_cons_self _ _, h_pred⟩
    · -- predicate false on x: recurse
      have := ih h
      exact ⟨List.mem_cons_of_mem _ this.1, this.2⟩

/-- **Theorem T1 (enabled gate).**

A fire intent that successfully dispatches into a `RequestSeed` implies
its underlying trigger is present *and* enabled in the active snapshot
at the time of the fire. This theorem locks the invariant that a
disabled schedule or event trigger cannot accept new work, even if a
stale scheduler tick races the reconcile publish.

The statement is conjunctive and **kind-dispatched** — there are three
admissible branches and they differ in what the snapshot must witness:

* **Schedule branch.** `intent.triggerKind = .schedule` requires an
  `activeSchedule` in `snap.activeSchedules` whose `triggerId` matches
  the intent and whose `enabled = true`. Conceptually: the clock-tick
  source may only produce a fire for a schedule the reconcile layer
  has published as active.
* **Event branch.** `intent.triggerKind = .event` requires an
  `activeEventTrigger` in `snap.activeEventTriggers` with matching
  `triggerId` and `enabled = true`. Stated over the concrete
  `ActiveEventTrigger` structure: the existential witness `trig` has
  the full shape `{triggerId, taskId, sourceCollection, eventKind,
  enabled, concurrency}`, matching the Rust `ResolvedEventTrigger`.
  The projection `trig.triggerId` / `trig.enabled` is what the gate
  checks; the remaining fields are load-bearing for `dispatch`'s
  operational definition (e.g. joining on `sourceCollection` and
  `eventKind` to decide which subscription's buffer to drain).
* **Manual branch — unconditional.** `intent.triggerKind = .manual`
  imposes **no snapshot precondition** on the gate. Manual fires are
  operator-initiated and do not reference any trigger document, so
  there is nothing to look up. The Rust mirror of this is
  `TriggerEngine::dispatch` falling through the `TriggerKind::Manual`
  arm without touching `snapshot.active_schedules` or
  `snapshot.active_event_triggers`. See `T1_manual_unconditional`
  below for the lemma form of this observation.

**Proof approach.** Unfold `dispatch`, case on `intent.triggerKind`,
then on `intent.triggerId`, then on the `dispatchEnabledForSchedule` /
`dispatchEnabledForEvent` lookup. The `find?_some_and_mem` helper
(below the proof block) extracts both membership in the active list
and the `(triggerId == tid) && enabled` predicate from
`List.find? = some active`, which combine to yield the existential
witness demanded by the conclusion. -/
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
  · -- Schedule branch
    intro h_kind
    unfold dispatch at h_dispatch
    rw [h_kind] at h_dispatch
    simp only at h_dispatch
    -- Case on intent.triggerId
    cases h_triggerId : intent.triggerId with
    | none =>
      rw [h_triggerId] at h_dispatch
      simp at h_dispatch
    | some tid =>
      rw [h_triggerId] at h_dispatch
      simp only at h_dispatch
      -- Case on dispatchEnabledForSchedule result
      cases h_found : dispatchEnabledForSchedule snap tid with
      | none =>
        rw [h_found] at h_dispatch
        simp at h_dispatch
      | some active =>
        -- Witness: active ∈ snap.activeSchedules, active satisfies the predicate.
        unfold dispatchEnabledForSchedule at h_found
        have ⟨h_mem, h_pred⟩ := find?_some_and_mem h_found
        -- h_pred : ((active.triggerId == tid) && active.enabled) = true
        have ⟨h_beq, h_enabled⟩ := (Bool.and_eq_true _ _).mp h_pred
        refine ⟨tid, rfl, active, h_mem, ?_, ?_⟩
        · exact beq_iff_eq.mp h_beq
        · exact h_enabled
  · -- Event branch — identical structure with EventTrigger / dispatchEnabledForEvent.
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

/-- **Lemma `T1_manual_unconditional`.**

Companion observation to `T1_enabled_gate`: the Manual branch is
*semantically distinct* from the Schedule and Event branches because
no snapshot precondition is imposed on it. A successful dispatch of a
Manual intent implies nothing about `snap.activeSchedules` or
`snap.activeEventTriggers` — not even existence.

The conclusion names the exact seed produced by the manual arm, making the
snapshot independence concrete instead of encoding it as a vacuous fact. -/
theorem T1_manual_unconditional
    (snap : TriggerSnapshot) (intent : FireIntent) :
    intent.triggerKind = .manual →
    dispatch snap intent =
      some { causedByTriggerId := none, causedByTriggerKind := .manual } := by
  intro h_kind
  unfold dispatch
  rw [h_kind]
