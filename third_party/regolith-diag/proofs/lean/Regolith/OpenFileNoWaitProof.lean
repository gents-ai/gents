-- The first half: the steps, the facts `Inv` and the lemmas used here.
import Regolith.OpenFileNoWait

/-!
# OpenFileNoWaitProof: every step keeps the facts, so no parked unit is lost

The second half of `Regolith/OpenFileNoWait.lean` (decision D60), which
defines the steps, the facts `Inv` and the lemmas used here. This file proves
that every step of the real code keeps every fact (`inv_step`), so every
reachable state has them (`inv_reach`), and from them the rule TLA+ calls
`RetryNeverLost` (`retry_never_lost`): a parked unit already has its "slot
freed" message, or its queue is on the parked list and some slot is WANTED
and busy, or a reader that freed a WANTED slot is on its way to waking the
list. It holds for every number of slots and threads.

Each step gets its own case below: the steps that touch nothing the facts
are about go through `inv_frame`; the others are checked fact by fact.
-/

-- Everything below is named Regolith.OpenFileNoWait.<name>.
namespace Regolith.OpenFileNoWait

-- Reuse the one-key update of `OpenFileTable.lean`.
open Regolith.OpenFileTable (set set_same set_other)

/-- Every step of the real code keeps every fact. -/
theorem inv_step {cap : Nat} {nw : Nat → Bool} {s s' : St} {u : Nat} (hs : Inv cap s)
    -- One step of thread `u`.
    (h : Step .real cap nw u s s') : Inv cap s' := by
  -- A thread that is not idle is not parked.
  have notparked : ∀ t, s.pc t ≠ .idle → s.parked t = false := by
    -- Take such a thread, and look at whether it is parked.
    intro t ht; cases hp : s.parked t
    -- It is not.
    · rfl
    -- It is: then it is idle, which `ht` rules out.
    · exact absurd ((hs.idle_iff t).1 hp) ht
  -- One case per step.
  cases h with
  -- A join: thread `u` joins an open slot as one more reader.
  | join _ _ j hpc hj hph =>
    -- Start to read: calm to calm; the joined slot is busy now.
    refine inv_frame (u := u) hs (by simp [hpc, Pc.calm]) (by simp [Pc.calm])
      -- Every other thread's step and slot are as they were.
      (fun r hr => by simp [set_apply, hr]) (fun r hr => by simp [set_apply, hr])
      -- Parking, messages, the list, looks and marks are unchanged; free slots are checked next.
      rfl rfl rfl rfl rfl ?_
    -- A marked free slot: not the joined one; any other is as it was.
    intro i _ hf
    -- Is it the joined slot?
    by_cases hij : i = j
    -- It is: it has a reader now, so it is not free.
    · subst hij; simp [St.free, hph] at hf
    -- It is not: as before.
    · simpa [St.free, set_apply, hij] using hf
  -- A miss: thread `u` goes to claim a slot.
  | miss _ _ hpc =>
    -- Start to claim: calm to calm, no slot changes.
    exact inv_frame (u := u) hs (by simp [hpc, Pc.calm]) (by simp [Pc.calm])
      -- Every other thread's step is as it was, and nobody's slot changed.
      (fun r hr => by simp [set_apply, hr]) (fun _ _ => rfl)
      -- Nothing else changed, and no slot became free.
      rfl rfl rfl rfl rfl (fun _ _ hf => hf)
  -- A claim: thread `u` takes a free slot with one CAS.
  | claim _ _ j hpc hj hfree =>
    -- Claim to opening: calm to calm; the claimed slot is not free now.
    refine inv_frame (u := u) hs (by simp [hpc, Pc.calm]) (by simp [Pc.calm])
      -- Every other thread's step and slot are as they were.
      (fun r hr => by simp [set_apply, hr]) (fun r hr => by simp [set_apply, hr])
      -- Parking, messages, the list, looks and marks are unchanged; free slots are checked next.
      rfl rfl rfl rfl rfl ?_
    -- A marked free slot: not the claimed one; any other is as it was.
    intro i _ hf
    -- Is it the claimed slot?
    by_cases hij : i = j
    -- It is: claimed, so not free.
    · subst hij; simp [St.free] at hf
    -- It is not: as before.
    · simpa [St.free, set_apply, hij] using hf
  -- A publish: thread `u` opens its table in its slot and reads.
  | open_ _ _ hpc =>
    -- Opening to read: calm to calm; the published slot has a reader.
    refine inv_frame (u := u) hs (by simp [hpc, Pc.calm]) (by simp [Pc.calm])
      -- Every other thread's step is as it was, and nobody's slot changed.
      (fun r hr => by simp [set_apply, hr]) (fun _ _ => rfl)
      -- Parking, messages, the list and looks are unchanged, and the real publish keeps every
      -- mark; free slots are checked next.
      rfl rfl rfl rfl (by simp) ?_
    -- A marked free slot: not the published one; any other is as it was.
    intro i _ hf
    -- Is it the published slot?
    by_cases hij : i = s.at_ u
    -- It is: open with a reader, so not free.
    · subst hij; simp [St.free] at hf
    -- It is not: as before.
    · simpa [St.free, set_apply, hij] using hf
  -- A leave: thread `u` stops reading its slot.
  | leave _ _ hpc =>
    -- Did this leave free a marked slot?
    by_cases hc : s.readers (s.at_ u) = 1 ∧ s.phase (s.at_ u) = .open_ ∧
        -- (and the slot is marked)
        s.wanted (s.at_ u) = true
    -- It did: `u` goes on to clear the mark.
    ·
      -- `u` is in the middle of a read, so it is not parked.
      have hp := notparked u (by simp [hpc])
      -- Each fact after the leave.
      refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
      -- Parked iff idle: `u` is neither.
      · intro t; by_cases htu : t = u
        -- `u`: not parked, and about to clear, not idle.
        · subst htu; simp [hc, hp]
        -- Anyone else: as before.
        · simpa [set_apply, htu] using hs.idle_iff t
      -- About to park: not `u`; anyone else as before.
      · intro t ht; by_cases htu : t = u
        -- `u` is about to clear.
        · subst htu; simp [hc] at ht
        -- Anyone else: as before.
        · simp [set_apply, htu] at ht; exact hs.park_done t ht
      -- Parked: as before.
      · exact hs.parked_done
      -- About to join the list: not `u`; anyone else as before.
      · intro t ht; by_cases htu : t = u
        -- `u` is about to clear.
        · subst htu; simp [hc] at ht
        -- Anyone else: as before.
        · simp [set_apply, htu] at ht; exact hs.register_zero t ht
      -- Marked and free: the freed slot has `u` on its way; others as before.
      · intro j hw hf
        -- Is it the slot `u` freed?
        by_cases hj : j = s.at_ u
        -- It is: `u` is about to clear its mark.
        · subst hj; exact ⟨u, by simp [hc], by simp⟩
        -- It is not: it was marked and free before.
        · have hf0 : s.free j = true := by simpa [St.free, set_apply, hj] using hf
          -- The reader on its way then.
          obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf0
          -- It is not `u`, which was reading.
          have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
          -- It is still there.
          exact ⟨r, by simp [set_apply, hru, hr], by simp [ha]⟩
      -- Covered: a relying thread is not `u`; a wake on its way stays.
      · intro t ht
        -- `t` is not `u`, which is about to clear.
        have htu : t ≠ u := by rintro rfl; simp [hc, hp] at ht
        -- `t` relied before the step too.
        have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
          -- Its step and its parking are as they were.
          simpa [set_apply, htu] using ht
        -- Nothing it relies on changed.
        exact covered_of (hs.relying t ht0) rfl rfl
          -- A wake on its way stays on its way.
          (waking_frame (u := u) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
          -- Its look and every mark are as they were.
          rfl (fun _ h => h)
    -- It did not: `u` goes back to its start, a calm step.
    ·
      -- A calm step keeps every fact once the conditions listed below are shown.
      refine inv_frame (u := u) hs (by simp [hpc, Pc.calm]) (by simp [hc, Pc.calm])
        -- Every other thread's step is as it was, and nobody's slot changed.
        (fun r hr => by simp [set_apply, hr]) (fun _ _ => rfl)
        -- Parking, messages, the list, looks and marks are unchanged; free slots are checked next.
        rfl rfl rfl rfl rfl ?_
      -- A marked slot free now was free before.
      intro j hw hf
      -- Is it the slot `u` left?
      by_cases hj : j = s.at_ u
      -- It is the slot `u` left.
      ·
        -- It is marked, so by `hc` it did not have exactly one open reader. Write it as `s.at_ u`.
        subst hj
        -- Unfold what free means, now and before.
        simp only [St.free, set_apply, ite_true] at hf ⊢
        -- Split on the slot's phase.
        cases hph : s.phase (s.at_ u) with
        -- Empty: free before as now.
        | empty => rfl
        -- Claimed: not free now either.
        | claimed => simp [hph] at hf
        -- Open: free now means at most one reader before; exactly one is ruled out.
        | open_ =>
          -- Free now: one reader or none before this leave.
          simp [hph] at hf
          -- Not exactly one: that, open and marked, is the case `hc` excludes.
          have hne : s.readers (s.at_ u) ≠ 1 := fun h1 => hc ⟨h1, hph, hw⟩
          -- So none before: free before too; the arithmetic closes it.
          simp; omega
      -- It is not: as before.
      · simpa [St.free, set_apply, hj] using hf
  -- A clear: thread `u` clears the mark of the slot it freed.
  | clearW _ _ hpc =>
    -- `u` was clearing, not parked; it goes on to wake.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the clear.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither.
    · intro t; by_cases htu : t = u
      -- `u`: not parked, about to wake.
      · subst htu; simp [hp]
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to wake.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: as before.
    · exact hs.parked_done
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to wake.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: not the cleared slot; another has its own reader.
    · intro j hw hf
      -- Is it the slot `u` cleared?
      by_cases hj : j = s.at_ u
      -- It is: its mark is gone.
      · subst hj; simp at hw
      -- It is not: marked and free before, with a reader on its way.
      · have hw0 : s.wanted j = true := by simpa [set_apply, hj] using hw
        -- The reader on its way then.
        obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw0 hf
        -- It is not `u`, which clears another slot.
        have hru : r ≠ u := by rintro rfl; exact hj ha.symm
        -- It is still there.
        exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: a relying thread is not `u`; `u` is now the wake on its way.
    · intro t ht
      -- `t` is not `u`, which is about to wake.
      have htu : t ≠ u := by rintro rfl; simp [hp] at ht
      -- `t` relied before the step too.
      have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
        -- Its step and its parking are as they were.
        simpa [set_apply, htu] using ht
      -- Take how it was covered.
      rcases hs.relying t ht0 with h | ⟨hl, _⟩
      -- Told: still told.
      · exact Or.inl h
      -- Listed: still listed, and `u` is the wake on its way.
      · exact Or.inr ⟨hl, Or.inl ⟨u, Or.inr (by simp)⟩⟩
  -- A wake: thread `u` leaves a message for every queue on the list.
  | wake _ _ hpc =>
    -- `u` was waking, not parked; it goes back to its start.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the wake.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither.
    · intro t; by_cases htu : t = u
      -- `u`: not parked, at its start.
      · subst htu; simp [hp]
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is at its start.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: as before.
    · exact hs.parked_done
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is at its start.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: no slot changed, and the reader on its way is not `u`.
    · intro j hw hf
      -- The reader on its way before.
      obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf
      -- It is not `u`, which was waking.
      have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
      -- It is still there.
      exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: every relying thread has its message now.
    · intro t ht
      -- `t` is not `u`, which is at its start.
      have htu : t ≠ u := by rintro rfl; simp [hp] at ht
      -- `t` relied before the step too.
      have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
        -- Its step and its parking are as they were.
        simpa [set_apply, htu] using ht
      -- The wake left it a message.
      exact Or.inl (covered_told (hs.relying t ht0))
  -- A park that claimed a slot wakes the list before filling it.
  | parkWake _ _ hpc =>
    -- `u` claimed a slot in its look and wakes the list; not parked.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the wake.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither.
    · intro t; by_cases htu : t = u
      -- `u`: not parked, about to open its table.
      · subst htu; simp [hp]
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to open.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: as before.
    · exact hs.parked_done
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to open.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: no slot changed, and the reader on its way is not `u`.
    · intro j hw hf
      -- The reader on its way before.
      obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf
      -- It is not `u`, which was waking the list.
      have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
      -- It is still there.
      exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: every relying thread has its message now.
    · intro t ht
      -- `t` is not `u`, which is about to open.
      have htu : t ≠ u := by rintro rfl; simp [hp] at ht
      -- `t` relied before the step too.
      have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
        -- Its step and its parking are as they were.
        simpa [set_apply, htu] using ht
      -- The wake left it a message.
      exact Or.inl (covered_told (hs.relying t ht0))
  -- A sweep that missed: the unit `u` goes to join the list.
  | sweepMiss _ _ hpc _ _ =>
    -- `u` wanted a slot, not parked; its sweep missed and it will register.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the miss.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither.
    · intro t; by_cases htu : t = u
      -- `u`: not parked, about to register.
      · subst htu; simp [hp]
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to register.
      · subst htu; simp at ht
      -- Anyone else: as before, look included.
      · simp [set_apply, htu] at ht ⊢; exact hs.park_done t ht
    -- Parked: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is not parked.
      · subst htu; simp [hp] at ht
      -- Anyone else: as before, look included.
      · simp [set_apply, htu]; exact hs.parked_done t ht
    -- About to join the list: `u`, whose look starts at slot 0.
    · intro t ht; by_cases htu : t = u
      -- `u`: its look was just set to 0.
      · subst htu; simp
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht ⊢; exact hs.register_zero t ht
    -- Marked and free: no slot changed, and the reader on its way is not `u`.
    · intro j hw hf
      -- The reader on its way before.
      obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf
      -- It is not `u`, which wanted a slot.
      have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
      -- It is still there.
      exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: a relying thread is not `u`, and nothing it relies on changed.
    · intro t ht
      -- `t` is not `u`, which is about to register.
      have htu : t ≠ u := by rintro rfl; simp [hp] at ht
      -- `t` relied before the step too.
      have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
        -- Its step and its parking are as they were.
        simpa [set_apply, htu] using ht
      -- Nothing it relies on changed.
      exact covered_of (hs.relying t ht0) rfl rfl
        -- A wake on its way stays on its way.
        (waking_frame (u := u) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
        -- Its look is as it was, and every mark stays.
        (by simp [set_apply, htu]) (fun _ h => h)
  -- A push: the unit `u` puts its queue on the list.
  | register _ _ hpc =>
    -- `u` joins the list and starts looking; not parked.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the push.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither.
    · intro t; by_cases htu : t = u
      -- `u`: not parked, looking.
      · subst htu; simp [hp]
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is looking.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: as before.
    · exact hs.parked_done
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is looking.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: no slot changed, and the reader on its way is not `u`.
    · intro j hw hf
      -- The reader on its way before.
      obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf
      -- It is not `u`, which was joining the list.
      have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
      -- It is still there.
      exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: `u` is listed with nothing looked at yet; others as before.
    · intro t ht; by_cases htu : t = u
      -- `u`: listed, and its look is at slot 0, so no mark is owed yet.
      · subst htu
        -- Its look starts at 0.
        have h0 := hs.register_zero t hpc
        -- Listed, with every mark it has looked at (none) in place.
        exact Or.inr ⟨by simp, Or.inr (fun j hj => by simp [h0] at hj)⟩
      -- Anyone else: relied before, and nothing it relies on changed.
      · have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
          -- Its step and its parking are as they were.
          simpa [set_apply, htu] using ht
        -- Its list entry is as it was.
        exact covered_of (hs.relying t ht0) rfl (by simp [set_apply, htu])
          -- A wake on its way stays on its way.
          (waking_frame (u := u) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
          -- Its look and every mark are as they were.
          rfl (fun _ h => h)
  -- A mark: the unit's look finds a busy slot and marks it.
  | markBusy _ _ hpc hm hbusy =>
    -- `u` marks the busy slot it looks at, and moves its look on.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the mark.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: nobody's step or parking changed.
    · exact hs.idle_iff
    -- About to park: not `u`, which looks; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is looking.
      · subst htu; simp [hpc] at ht
      -- Anyone else: as before.
      · simp [set_apply, htu]; exact hs.park_done t ht
    -- Parked: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is not parked.
      · subst htu; simp [hp] at ht
      -- Anyone else: as before.
      · simp [set_apply, htu]; exact hs.parked_done t ht
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is looking.
      · subst htu; simp [hpc] at ht
      -- Anyone else: as before.
      · simp [set_apply, htu]; exact hs.register_zero t ht
    -- Marked and free: the newly marked slot is busy; others as before.
    · intro j hw hf
      -- Is it the slot `u` just marked?
      by_cases hj : j = s.markAt u
      -- It is: it is busy, not free.
      · subst hj; simp [St.free] at hf hbusy; rw [hf] at hbusy; cases hbusy
      -- It is not: marked and free before.
      · have hw0 : s.wanted j = true := by simpa [set_apply, hj] using hw
        -- The reader on its way then is still there.
        exact hs.free_wanted j hw0 hf
    -- Covered: `u` keeps its marks and adds one; others only gain a mark.
    · intro t ht; by_cases htu : t = u
      -- `u`: covered before, as a looking unit.
      · subst htu
        -- How it was covered.
        rcases hs.relying t (Or.inl hpc) with h | ⟨hl, hw | hw⟩
        -- Told: still told.
        · exact Or.inl h
        -- Listed with a wake on its way: still.
        · exact Or.inr ⟨hl, Or.inl hw⟩
        -- Listed with its marks: those stay, and the new slot is marked too.
        · refine Or.inr ⟨hl, Or.inr ?_⟩
          -- Each slot before the moved look.
          intro j hj
          -- Is it the slot just marked?
          by_cases hjm : j = s.markAt t
          -- It is: just marked.
          · subst hjm; simp
          -- It is not: marked before, and still.
          · have hlt : j < s.markAt t := by simp at hj; omega
            -- The mark it had before is untouched by the new one.
            simp [set_apply, hjm, hw j hlt]
      -- Anyone else: relied before, and only a mark was added.
      · have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := ht
        -- Nothing else it relies on changed.
        refine covered_of (hs.relying t ht0) rfl rfl (fun h => h) (by simp [set_apply, htu]) ?_
        -- A mark stays a mark.
        intro j hj; by_cases hjm : j = s.markAt u
        -- The newly marked slot.
        · subst hjm; simp
        -- Any other slot.
        · simp [set_apply, hjm, hj]
  -- A claim in the look: the unit finds a free slot and takes it.
  | markClaim _ _ hpc hm hfree =>
    -- `u`'s look claims a free slot; it goes on to wake the list.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the claim.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither.
    · intro t; by_cases htu : t = u
      -- `u`: not parked, about to wake.
      · subst htu; simp [hp]
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to wake.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: as before.
    · exact hs.parked_done
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to wake.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: not the claimed slot; others as before.
    · intro j hw hf
      -- Is it the claimed slot?
      by_cases hj : j = s.markAt u
      -- It is: claimed, not free.
      · subst hj; simp [St.free] at hf
      -- It is not: marked and free before.
      · have hf0 : s.free j = true := by simpa [St.free, set_apply, hj] using hf
        -- The reader on its way then.
        obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf0
        -- It is not `u`, which was looking.
        have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
        -- It is still there.
        exact ⟨r, by simp [set_apply, hru, hr], by simp [set_apply, hru, ha]⟩
    -- Covered: a relying thread is not `u`, and nothing it relies on changed.
    · intro t ht
      -- `t` is not `u`, which is about to wake.
      have htu : t ≠ u := by rintro rfl; simp [hp] at ht
      -- `t` relied before the step too.
      have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
        -- Its step and its parking are as they were.
        simpa [set_apply, htu] using ht
      -- Nothing it relies on changed.
      exact covered_of (hs.relying t ht0) rfl rfl
        -- A wake on its way stays on its way.
        (waking_frame (u := u) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
        -- Its look and every mark are as they were.
        rfl (fun _ h => h)
  -- The look is done: every slot was busy, and the unit goes to park.
  | markDone _ _ hpc hm =>
    -- `u` looked at every slot and goes to park.
    have hp := notparked u (by simp [hpc])
    -- Each fact after the look.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither.
    · intro t; by_cases htu : t = u
      -- `u`: not parked, about to park.
      · subst htu; simp [hp]
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: `u` has looked at every slot; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u`: its look is past the last slot.
      · subst htu; exact hm
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: as before.
    · exact hs.parked_done
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is about to park.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: no slot changed, and the reader on its way is not `u`.
    · intro j hw hf
      -- The reader on its way before.
      obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf
      -- It is not `u`, which was looking.
      have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
      -- It is still there.
      exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: `u` was covered while looking; others as before.
    · intro t ht; by_cases htu : t = u
      -- `u`: covered while looking, and nothing it relies on changed.
      · subst htu
        -- It was covered, and its message and list entry are as they were.
        exact covered_of (hs.relying t (Or.inl hpc)) rfl rfl
          -- A wake on its way stays on its way.
          (waking_frame (u := t) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
          -- Its look and every mark are as they were.
          rfl (fun _ h => h)
      -- Anyone else: relied before, and nothing it relies on changed.
      · have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
          -- Its step and its parking are as they were.
          simpa [set_apply, htu] using ht
        -- It was covered, and its message and list entry are as they were.
        exact covered_of (hs.relying t ht0) rfl rfl
          -- A wake on its way stays on its way.
          (waking_frame (u := u) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
          -- Its look and every mark are as they were.
          rfl (fun _ h => h)
  -- A park: the unit is parked and its poll returns.
  | park _ _ hpc =>
    -- `u` parks and its poll returns.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is both now; anyone else as before.
    · intro t; by_cases htu : t = u
      -- `u`: parked and idle.
      · subst htu; simp
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`, which is idle; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is idle.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: `u` looked at every slot before parking; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u`: it was about to park, so its look was done.
      · subst htu; exact hs.park_done t hpc
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.parked_done t ht
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is idle.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: no slot changed, and the reader on its way is not `u`.
    · intro j hw hf
      -- The reader on its way before.
      obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf
      -- It is not `u`, which was about to park.
      have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
      -- It is still there.
      exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: `u` was covered about to park; others as before.
    · intro t ht; by_cases htu : t = u
      -- `u`: covered about to park, and nothing it relies on changed.
      · subst htu
        -- It was covered, and its message and list entry are as they were.
        exact covered_of (hs.relying t (Or.inr (Or.inl hpc))) rfl rfl
          -- A wake on its way stays on its way.
          (waking_frame (u := t) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
          -- Its look and every mark are as they were.
          rfl (fun _ h => h)
      -- Anyone else: relied before, and nothing it relies on changed.
      · have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
          -- Its step and its parking are as they were.
          simpa [set_apply, htu] using ht
        -- It was covered, and its message and list entry are as they were.
        exact covered_of (hs.relying t ht0) rfl rfl
          -- A wake on its way stays on its way.
          (waking_frame (u := u) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
          -- Its look and every mark are as they were.
          rfl (fun _ h => h)
  -- A poll: the unit takes its message and runs again.
  | poll _ _ hpc htold =>
    -- `u` takes its message and runs its unit again.
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
    -- Parked iff idle: `u` is neither now; anyone else as before.
    · intro t; by_cases htu : t = u
      -- `u`: unparked, at its start.
      · subst htu; simp
      -- Anyone else: as before.
      · simpa [set_apply, htu] using hs.idle_iff t
    -- About to park: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is at its start.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.park_done t ht
    -- Parked: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is not parked any more.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.parked_done t ht
    -- About to join the list: not `u`; anyone else as before.
    · intro t ht; by_cases htu : t = u
      -- `u` is at its start.
      · subst htu; simp at ht
      -- Anyone else: as before.
      · simp [set_apply, htu] at ht; exact hs.register_zero t ht
    -- Marked and free: no slot changed, and the reader on its way is not `u`.
    · intro j hw hf
      -- The reader on its way before.
      obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf
      -- It is not `u`, which was idle.
      have hru : r ≠ u := by rintro rfl; simp [hpc] at hr
      -- It is still there.
      exact ⟨r, by simp [set_apply, hru, hr], ha⟩
    -- Covered: a relying thread is not `u`, and its message is as it was.
    · intro t ht
      -- `t` is not `u`, which is at its start and unparked.
      have htu : t ≠ u := by rintro rfl; simp at ht
      -- `t` relied before the step too.
      have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
        -- Its step and its parking are as they were.
        simpa [set_apply, htu] using ht
      -- Nothing it relies on changed.
      exact covered_of (hs.relying t ht0) (by simp [set_apply, htu]) rfl
        -- A wake on its way stays on its way.
        (waking_frame (u := u) (by simp [hpc]) (fun r hr => by simp [set_apply, hr]))
        -- Its look and every mark are as they were.
        rfl (fun _ h => h)

/-- Every reachable state of the real code has every fact. -/
theorem inv_reach {cap : Nat} {nw : Nat → Bool} {s : St} (h : Reach .real cap nw s) :
    -- The claim itself; its proof follows.
    Inv cap s := by
  -- By the number of steps.
  induction h with
  -- The start has every fact.
  | init => exact inv_init cap
  -- One more step keeps them.
  | step _ _ hstep ih => exact inv_step ih hstep

/-- **`retry_never_lost`.** In the real code, with at least one slot, a
parked unit already has its message, or its queue is on the parked list and
either some slot is WANTED and busy (the read that frees it wakes the list)
or a reader that freed a WANTED slot is on its way to waking it. TLA+
`RetryNeverLost`. -/
theorem retry_never_lost {cap : Nat} {nw : Nat → Bool} {s : St} (hcap : 0 < cap)
    -- A reachable state, and a parked unit `t`.
    (h : Reach .real cap nw s) (t : Nat) (ht : s.parked t = true) :
    -- The claim: told, or listed with a busy marked slot or a wake on its way.
    s.told t = true ∨ (s.list t = true ∧
      -- (a busy, marked slot below the limit, or a reader on its way to wake)
      ((∃ j, j < cap ∧ s.wanted j = true ∧ s.free j = false) ∨ s.waking)) := by
  -- Every fact holds there.
  have hs := inv_reach h
  -- `t` is covered, and has looked at every slot.
  rcases hs.relying t (Or.inr (Or.inr ht)) with htold | ⟨hl, hw | hw⟩
  -- Told: done.
  · exact Or.inl htold
  -- Listed with a wake on its way: done.
  · exact Or.inr ⟨hl, Or.inr hw⟩
  -- Listed with every slot it looked at marked: slot 0 is marked.
  · have h0 : s.wanted 0 = true := hw 0 (by have := hs.parked_done t ht; omega)
    -- Is slot 0 free?
    cases hf : s.free 0
    -- Busy and marked: its free will wake the list.
    · exact Or.inr ⟨hl, Or.inl ⟨0, hcap, h0, hf⟩⟩
    -- Free and marked: a reader is on its way to clearing it.
    · obtain ⟨r, hr, _⟩ := hs.free_wanted 0 h0 hf
      -- That reader is the wake on its way.
      exact Or.inr ⟨hl, Or.inr ⟨r, Or.inl hr⟩⟩

-- The end of this file's names.
end Regolith.OpenFileNoWait
