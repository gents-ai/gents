-- States, writes, the read classes and their rules come from Validation.lean.
import Regolith.Validation

/-!
# Relaxations: what each key class gives up, stated exactly (plan 3.1 to 3.15)

This file continues `Regolith/Validation.lean` and backs the same TLA+ model,
`proofs/tla/RepeatableRead.tla`. Validation.lean proves that every class's
rule gives a history serial in commit order. Here each class's declared
relaxation is stated exactly, with the RED cases as counterexamples. (In the
TLA+ model a GREEN configuration runs the rule as the engine has it and must
break no promise in any order of events; a RED one plants one bug or misuse
and must break the promise it names. A RED case here is the small worked
counterexample that shows, in numbers, why that promise breaks.)

* `blindMerge_commutes` (configuration `MC_DefraLevel_Green_Counters`,
  invariant `INV_IncrementsCommitUnlessReplaced`): with commuting operands, a
  blind merge that passed its rule may be serialized at its snapshot as well
  as at its commit; `blindMerge_across_replacement` is why the rule refuses a
  newer replacement (`MC_DefraLevel_Red_MergeIgnoresReplacement`).
* `identical_rewrite` (`MC_DefraLevel_Green_ValueReads`,
  `INV_IdenticalRewritesCommit`): an identical rewrite passes the value rule
  and fails the sequence rule (`MC_DefraLevel_Red_SeqOnlyValidation`).
* `commutative_union` (`MC_DefraLevel_Green_Heads`, `INV_HeadsExact`):
  writers that add their own keys and remove keys they saw leave the same
  state in either commit order.
* `appends_advance_head` and `appends_keep_dense` (`MC_DefraLevel_Green_Log`,
  `INV_LogDense`): each append takes the next position at its commit, so
  positions are dense from 1 and follow commit order.
* `writeFree_at_snapshot` (`MC_DefraLevel_Green_WriteFree`,
  `INV_WriteFreeConsistent`): what a transaction read is the serial state at
  its snapshot, so a write-free one serializes there.
* `exempt_decision_breaks_serial` (`MC_DefraLevel_Red_DecideOnLogRead`,
  `MC_DefraLevel_Red_PartsNamesOther`), `promoted_read_skew`
  (`MC_DefraLevel_Red_WriteFreePessimistic`) and `read_only_anomaly`
  (`MC_Serializable_Red_ReadOnlyAnomaly`, `MC_SnapshotIsolation_Red_ReadOnlyAnomaly`):
  the RED cases.

## The story, for a reader who has never seen the code

A "relaxation" is a check the engine deliberately skips for one class of
key, because the caller promised something about that class. Each one
trades a refusal that would be needless for a promise the caller must keep.

Tiny example: two transactions each add one to a counter with a merge and
read nothing. A strict check would refuse the second, because the counter
changed under it. But "add one" twice gives the same counter in either
order, so both may commit: that is the blind-merge relaxation. What could go
wrong: a relaxation applied where its promise does not hold. A reset of the
counter between the two is such a case, and the rule refuses it.

This file states, for each class, exactly what stays true when the check is
skipped, and shows with small counterexamples what breaks when a caller
leans on more than that.

How to read the Lean: as in Validation.lean; every proof step is commented.
-/

-- This file adds to the names of Regolith.Validation.
namespace Regolith.Validation

/-! ## The relaxations, stated exactly -/

/-- The blind-merge rule: every newer write of `k` is an operand, so no
replacement landed since the snapshot (`RegolithEngine::landed_above`,
src/engine/commit/terminator.rs). -/
def BlindMergeOk (E : Env) (k : Nat) (ws : List Write) : Prop :=
  -- For every newer write that affects k: it is a merge of some operand into k.
  ∀ w ∈ ws, affects E k w = true → ∃ o, w = .merge k o

/-- A merge into `k` leaves on `k` the operand applied to what `k` held. -/
theorem merge_at (E : Env) (σ : State) (k o : Nat) :
    -- The statement: after the merge, k holds the operator's fold of o onto what k held.
    applyWrite E σ (.merge k o) k = E.op.apply (σ k) o := by
  -- Unfold the merge: the update is at k.
  simp [applyWrite, upd]

/-- When every write affecting `k` is an operand on `k`, the value of `k`
after the writes depends only on the value of `k` before them. -/
theorem merges_at (E : Env) {k : Nat} :
    -- The statement: for every list of writes and every two states that agree on k, if every write
    -- affecting k is an operand on k, the two states still agree on k afterwards.
    ∀ (ws : List Write) (σ τ : State), BlindMergeOk E k ws → σ k = τ k →
      applyWrites E σ ws k = applyWrites E τ ws k
  -- No writes: they agree as given.
  | [], _, _, _, h => h
  -- A first write w and the rest ws.
  | w :: ws, σ, τ, hok, h => by
    -- Peel the first write off, on both sides.
    simp only [applyWrites_cons]
    -- It is enough that the two states after the first write agree on k: the rest follows from the
    -- claim for the shorter list.
    apply merges_at E ws _ _ (fun w' hw' => hok w' (List.mem_cons_of_mem _ hw'))
    -- The first write leaves `k` on both sides, or folds the same operand
    -- onto the same value.
    -- Split on whether the first write affects k.
    cases ha : affects E k w with
    -- It does not: k is unchanged on both sides, and they agreed.
    | false => rw [applyWrite_unaffected E σ w k ha, applyWrite_unaffected E τ w k ha, h]
    -- It does.
    | true =>
      -- Then it is a merge of some operand o into k.
      obtain ⟨o, rfl⟩ := hok w List.mem_cons_self ha
      -- The same operand folded onto the same value, on both sides.
      rw [merge_at, merge_at, h]

/-- **Blind merges commute.** With operands that commute, a blind merge that
passed its rule leaves `k` the same whether it is placed at its commit
(after the operands it missed) or at its snapshot (before them). -/
theorem blindMerge_commutes (E : Env)
    -- The operator's operands commute.
    (hcomm : ∀ v a b, E.op.apply (E.op.apply v a) b = E.op.apply (E.op.apply v b) a)
    -- (for any key and operand)
    (k o : Nat) :
    -- The statement: for every list of missed writes that passes the blind-merge rule, merging
    -- after them gives on k the same as merging before them.
    ∀ (ws : List Write) (σ : State), BlindMergeOk E k ws →
      applyWrite E (applyWrites E σ ws) (.merge k o) k =
        applyWrites E (applyWrite E σ (.merge k o)) ws k
  -- Nothing missed: the two placements are the same.
  | [], σ, _ => rfl
  -- A first missed write w and the rest ws.
  | w :: ws, σ, hok => by
    -- The rest passes the rule too.
    have hok' : BlindMergeOk E k ws := fun w' hw' => hok w' (List.mem_cons_of_mem _ hw')
    -- Place the merge before the rest of the missed writes (the claim for
    -- the shorter list).
    -- The claim for the shorter list, starting from the state after w.
    have ih := blindMerge_commutes E hcomm k o ws (applyWrite E σ w) hok'
    -- Peel the first write off, in that fact and in the goal.
    simp only [applyWrites_cons] at ih ⊢
    -- Rewrite the goal with it: the merge now sits right after w.
    rw [ih]
    -- Then swap it with the first missed write. Only `k` matters to the rest
    -- (`merges_at`), and on `k` the first write either does nothing or is an
    -- operand, which commutes with ours.
    -- Only k matters to the rest, so it is enough to compare k after "w then the merge" and after
    -- "the merge then w".
    apply merges_at E ws _ _ hok'
    -- Split on whether w affects k.
    cases ha : affects E k w with
    -- It does not.
    | false =>
      -- Then w leaves k on both sides, and the merge folds onto the same value.
      rw [merge_at, applyWrite_unaffected E σ w k ha,
        applyWrite_unaffected E _ w k ha, merge_at]
    -- It does.
    | true =>
      -- Then w is itself a merge of some operand o' into k.
      obtain ⟨o', rfl⟩ := hok w List.mem_cons_self ha
      -- Both orders unfold to two folds, one of o and one of o'.
      rw [merge_at, merge_at, merge_at, merge_at]
      -- Operands commute, so the two orders agree.
      exact hcomm _ _ _

/-- **An identical rewrite is not a change (plan 3.5).** A read that found
`v`, with a newer put of the same `v`, passes the value rule and fails the
sequence rule. -/
theorem identical_rewrite (E : Env) (σ : State) (k : Nat) (hv : σ k ≠ 0) :
    -- The statement: with one newer put of the same value, the value rule passes and the sequence
    -- rule fails.
    Ok E (.value k) σ [.put k (σ k)] ∧ ¬ Ok E (.full k) σ [.put k (σ k)] := by
  -- Two things to show.
  refine ⟨?_, ?_⟩
  -- The value rule: the newest write of `k` is the put itself, of the value read.
  ·
    -- Unfold the rule on this one-write list: the put leaves exactly the value read.
    simp [Ok, lastWhere, affects, SetsTo, hv]
  -- The sequence rule: any newer write of `k` fails it.
  ·
    -- Unfold the rule: a newer write of k exists, so "no newer write" is false.
    simp [Ok, lastWhere, affects]

/-- Puts and deletes of disjoint keys commute: applying two writers' lists in
either order leaves the same state. -/
def PutDel (w : Write) : Prop := (∃ k v, w = .put k v) ∨ ∃ k, w = .del k

/-- A put or a delete leaves the same value on its key from any state. -/
theorem putDel_const (E : Env) {w : Write}
    -- `w` is a put or a delete, and it affects `k`.
    (hw : PutDel w) {k : Nat}
    -- (it affects k, from any two states) The statement: it leaves the same value on k from both.
    (ha : affects E k w = true) (σ τ : State) : applyWrite E σ w k = applyWrite E τ w k := by
  -- Split: a put of some key j, or a delete of some key j.
  rcases hw with ⟨j, v, rfl⟩ | ⟨j, rfl⟩
  -- A put: it affects k, so j is k; it stores its value whatever the state was.
  · simp only [affects, decide_eq_true_eq] at ha; subst ha; simp [applyWrite, upd]
  -- A delete: it affects k, so j is k; it leaves 0 whatever the state was.
  · simp only [affects, decide_eq_true_eq] at ha; subst ha; simp [applyWrite, upd]

/-- After a list of puts and deletes, a key's value depends only on whether
the list affects it: if it does, not on the state the list started from. -/
theorem putDels_at (E : Env) {k : Nat} :
    -- The statement: for every list of puts and deletes one of which affects k, k's value
    -- afterwards is the same from any two starting states.
    ∀ (ws : List Write), (∀ w ∈ ws, PutDel w) → (∃ w ∈ ws, affects E k w = true) →
      ∀ σ τ, applyWrites E σ ws k = applyWrites E τ ws k := by
  -- Take the list, the two facts and the two states.
  intro ws hws hex σ τ
  -- Split on the newest write that affects k.
  cases hl : lastWhere (affects E k) ws with
  -- Case: none found.
  | none =>
    -- But some write does affect k.
    obtain ⟨w, hw, ha⟩ := hex
    -- The walk found none, so that write does not affect k.
    rw [lastWhere_none hl w hw] at ha
    -- A contradiction: it both does and does not.
    cases ha
  -- Case: it is w.
  | some w =>
    -- Split the list into a prefix, w, and later writes that do not affect k.
    obtain ⟨hq, pre, post, rfl, hpost⟩ := lastWhere_some hl
    -- Apply the prefix, then the rest, on both sides.
    rw [applyWrites_append, applyWrites_append]
    -- Then w, then the later writes.
    simp only [applyWrites_cons]
    -- The later writes leave k alone on both sides.
    rw [applyWrites_unaffected E k post _ hpost, applyWrites_unaffected E k post _ hpost]
    -- And w, a put or a delete on k, leaves the same value from any state.
    exact putDel_const E (hws w (by simp)) hq _ _

/-- **Concurrent writers in a commutative prefix leave the union.** Two
commits whose writes are puts and deletes of disjoint keys (each writer adds
its own keys and removes keys it observed) leave the same state in either
commit order. -/
theorem commutative_union (E : Env) (a b : List Write)
    -- Both writers only put and delete.
    (ha : ∀ w ∈ a, PutDel w) (hb : ∀ w ∈ b, PutDel w)
    -- No key is affected by both.
    (hdisj : ∀ k, (∃ w ∈ a, affects E k w = true) → ∀ w ∈ b, affects E k w = false)
    -- (from any state)
    (σ : State) :
    -- The statement: a then b leaves the same state as b then a.
    applyWrites E (applyWrites E σ a) b = applyWrites E (applyWrites E σ b) a := by
  -- Compare the two states key by key.
  funext k
  -- Split on whether some write of a affects k.
  by_cases hka : ∃ w ∈ a, affects E k w = true
  -- `a` decides `k`, and `b` leaves it.
  ·
    -- On the left, b (applied last) leaves k alone.
    rw [applyWrites_unaffected E k b _ (hdisj k hka)]
    -- So both sides are what a leaves on k, which does not depend on the state a started from.
    exact putDels_at E a ha hka σ _
  -- `a` leaves `k`; then `b` decides it or leaves it too.
  ·
    -- No write of a affects k:
    have hna : ∀ w ∈ a, affects E k w = false := by
      -- take any write of a;
      intro w hw
      -- split on whether it affects k;
      cases h : affects E k w with
      -- it does not: that is the claim;
      | false => rfl
      -- it does: but we are in the case where none does.
      | true => exact absurd ⟨w, hw, h⟩ hka
    -- On the right, a (applied last) leaves k alone.
    rw [applyWrites_unaffected E k a _ hna]
    -- Split on whether some write of b affects k.
    by_cases hkb : ∃ w ∈ b, affects E k w = true
    -- One does: both sides are what b leaves on k, whatever state b started from.
    · exact putDels_at E b hb hkb _ σ
    -- None does:
    · have hnb : ∀ w ∈ b, affects E k w = false := by
        -- take any write of b;
        intro w hw
        -- split on whether it affects k;
        cases h : affects E k w with
        -- it does not: that is the claim;
        | false => rfl
        -- it does: but none does.
        | true => exact absurd ⟨w, hw, h⟩ hkb
      -- So neither list touches k: both sides are σ k.
      rw [applyWrites_unaffected E k b _ hnb, applyWrites_unaffected E k b _ hnb,
        applyWrites_unaffected E k a _ hna]

/-! ## Log appends are commit-ordered -/

/-- A write keeps the Log contract: on a Log key, only an append of an entry
writes (plan 3.6: a transaction's own put or delete of a Log key is
refused). -/
def LogSafe (E : Env) (w : Write) : Prop :=
  -- For every Log key the write affects: the write is an append of a nonzero entry.
  ∀ k, E.isLog k = true → affects E k w = true → ∃ e, w = .append e ∧ e ≠ 0

/-- One for an append, zero for any other write. -/
def isAppend : Write → Nat
  -- An append counts one.
  | .append _ => 1
  -- A put counts zero.
  | .put _ _ => 0
  -- A delete counts zero.
  | .del _ => 0
  -- A merge counts zero.
  | .merge _ _ => 0
  -- A range delete counts zero.
  | .rangeDel _ _ => 0

/-- The number of appends in a list of writes. -/
def appends : List Write → Nat
  -- No writes: no appends.
  | [] => 0
  -- The first write's count plus the count of the rest.
  | w :: ws => isAppend w + appends ws

/-- **Appends advance the head one position each, in commit order.** Writes
that keep the Log contract move the head by exactly the number of appends
among them: each append takes the next position when it is applied. -/
theorem appends_advance_head (E : Env) :
    -- The statement: for every list of writes that keep the Log contract, the head afterwards is
    -- the head before plus the number of appends in the list.
    ∀ (ws : List Write) (σ : State), (∀ w ∈ ws, LogSafe E w) →
      applyWrites E σ ws E.head = σ E.head + appends ws
  -- No writes: the head has not moved.
  | [], σ, _ => rfl
  -- A first write w and the rest ws.
  | w :: ws, σ, hws => by
    -- Peel the first write off.
    simp only [applyWrites_cons] at *
    -- Use the claim for the rest: it adds the appends of ws to the head after w.
    rw [appends_advance_head E ws _ (fun w' hw' => hws w' (List.mem_cons_of_mem _ hw'))]
    -- Split on the kind of the first write.
    cases w with
    -- Case: an append.
    | append e =>
      -- The append sets the head to one more.
      -- Unfold: the head after it is one more, and it counts one. Arithmetic closes the goal.
      simp [applyWrite, upd, appends, isAppend] <;> omega
    -- Case: a put.
    | put k v =>
      -- Anything else leaves the head: a write of it would have to be an
      -- append.
      -- It does not affect the head:
      have : affects E E.head (.put k v) = false := by
        -- split on whether it does;
        cases h : affects E E.head (.put k v) with
        -- it does not: that is the claim;
        | false => rfl
        -- it does: the Log contract would make it an append, and a put is not one.
        | true => obtain ⟨_, he, _⟩ := hws _ List.mem_cons_self E.head E.isLog_head h; cases he
      -- So the head is unchanged, and a put counts zero.
      rw [applyWrite_unaffected E σ _ _ this]; simp [appends, isAppend]
    -- Case: a delete.
    | del k =>
      -- It does not affect the head:
      have : affects E E.head (.del k) = false := by
        -- split on whether it does;
        cases h : affects E E.head (.del k) with
        -- it does not: that is the claim;
        | false => rfl
        -- it does: the Log contract would make it an append, and a delete is not one.
        | true => obtain ⟨_, he, _⟩ := hws _ List.mem_cons_self E.head E.isLog_head h; cases he
      -- So the head is unchanged, and a delete counts zero.
      rw [applyWrite_unaffected E σ _ _ this]; simp [appends, isAppend]
    -- Case: a merge.
    | merge k o =>
      -- It does not affect the head:
      have : affects E E.head (.merge k o) = false := by
        -- split on whether it does;
        cases h : affects E E.head (.merge k o) with
        -- it does not: that is the claim;
        | false => rfl
        -- it does: the Log contract would make it an append, and a merge is not one.
        | true => obtain ⟨_, he, _⟩ := hws _ List.mem_cons_self E.head E.isLog_head h; cases he
      -- So the head is unchanged, and a merge counts zero.
      rw [applyWrite_unaffected E σ _ _ this]; simp [appends, isAppend]
    -- Case: a range delete.
    | rangeDel lo hi =>
      -- It does not affect the head:
      have : affects E E.head (.rangeDel lo hi) = false := by
        -- split on whether it does;
        cases h : affects E E.head (.rangeDel lo hi) with
        -- it does not: that is the claim;
        | false => rfl
        -- it does: the Log contract would make it an append, and a range delete is not one.
        | true => obtain ⟨_, he, _⟩ := hws _ List.mem_cons_self E.head E.isLog_head h; cases he
      -- So the head is unchanged, and a range delete counts zero.
      rw [applyWrite_unaffected E σ _ _ this]; simp [appends, isAppend]

/-- The log is dense: every position from 1 to the head holds an entry. -/
def LogDense (E : Env) (σ : State) : Prop :=
  -- For every position p from 1 to the head, the entry key of p holds something.
  ∀ p, 1 ≤ p → p ≤ σ E.head → σ (E.entry p) ≠ 0

/-- **Appends keep the log dense.** Writes that keep the Log contract, from a
dense log, leave a dense log: each append fills the position just past the
head and moves the head onto it, so no position is skipped. -/
theorem appends_keep_dense (E : Env) :
    -- The statement: for every list of writes that keep the Log contract, a dense log stays dense.
    ∀ (ws : List Write) (σ : State), LogDense E σ → (∀ w ∈ ws, LogSafe E w) →
      LogDense E (applyWrites E σ ws)
  -- No writes: dense as given.
  | [], σ, hd, _ => hd
  -- A first write w and the rest ws.
  | w :: ws, σ, hd, hws => by
    -- Peel the first write off.
    simp only [applyWrites_cons]
    -- It is enough that the log is dense after the first write: the rest follows from the claim for
    -- the shorter list.
    apply appends_keep_dense E ws _ _ (fun w' hw' => hws w' (List.mem_cons_of_mem _ hw'))
    -- Take any position p from 1 to the head after w.
    intro p hp1 hp
    -- Split on whether w affects the head.
    cases ha : affects E E.head w with
    -- Case: it does not.
    | false =>
      -- The head does not move, and no entry changes: a write of an entry
      -- would be an append, which moves the head.
      -- Then the head after w is the head before, so p is at or below the old head.
      rw [applyWrite_unaffected E σ w _ ha] at hp
      -- w does not affect entry p either:
      have he : affects E (E.entry p) w = false := by
        -- split on whether it does;
        cases h : affects E (E.entry p) w with
        -- it does not: that is the claim;
        | false => rfl
        -- it does:
        | true =>
          -- then the Log contract makes w an append,
          obtain ⟨e, rfl, _⟩ := hws w List.mem_cons_self _ (E.isLog_entry p) h
          -- and an append affects the head, against this case.
          simp [affects, E.isLog_head] at ha
      -- So entry p is what it was,
      rw [applyWrite_unaffected E σ w _ he]
      -- and the log was dense there.
      exact hd p hp1 hp
    -- Case: it does.
    | true =>
      -- An append: the new head is one more, its entry is the new one, and
      -- every earlier position keeps its entry.
      -- Then the Log contract makes w an append of a nonzero entry e.
      obtain ⟨e, rfl, he0⟩ := hws w List.mem_cons_self _ E.isLog_head ha
      -- The head after it is the old head plus one, so p is at most that.
      simp [applyWrite, upd] at hp
      -- Entry p's key is not the head's key.
      have hne : E.entry p ≠ E.head := E.entry_ne_head p
      -- Unfold the append at entry p: the head's update misses it.
      simp only [applyWrite, upd, hne]
      -- Split on whether p is the new position.
      by_cases hlast : p = σ E.head + 1
      -- It is: the entry stored there is e, which is not zero.
      · subst hlast; simp [he0]
      -- It is not: then p is at or below the old head.
      · have hlt : p ≤ σ E.head := by omega
        -- Entry p's key is not the new entry's key (different positions, different keys).
        have hneq : E.entry p ≠ E.entry (σ E.head + 1) := fun h => hlast (E.entry_inj _ _ h)
        -- So the append's update misses entry p.
        simp only [hneq]
        -- And the log was dense there before.
        exact hd p hp1 hlt

/-! ## Write-free transactions read one point in time -/

/-- A valid history's suffixes are valid. -/
-- The statement: for every valid history and every m, the history without its m newest commits is
-- valid too.
theorem valid_drop (E : Env) : ∀ (h : List Txn) (m : Nat), Valid E h → Valid E (h.drop m)
  -- Dropping none: the same history.
  | h, 0, hv => by simpa using hv
  -- Nothing to drop from: the empty history is valid.
  | [], _ + 1, _ => by simp [Valid]
  -- Dropping the newest and m more.
  | _ :: h, m + 1, hv => by
    -- Unfold valid: the older history is valid.
    simp only [Valid] at hv
    -- Drop m from the older history: the claim for it.
    simpa using valid_drop E h m hv.1

/-- **Write-free transactions (plan 3.8).** What any transaction read is the
serial state at its snapshot. A transaction with no writes changes nothing,
so it serializes there although it validated nothing. -/
theorem writeFree_at_snapshot (E : Env) (hinit : CAConsistent E E.init) {T : Txn}
    -- (the transaction is the newest commit of a valid history)
    {h : List Txn} (hv : Valid E (T :: h)) :
    -- The statement: the state it read equals the serial state of the commits it saw.
    run E (h.drop T.missed) = serial E (h.drop T.missed) := by
  -- Unfold valid: the older history is valid.
  simp only [Valid] at hv
  -- Its suffix is valid too, so the combined theorem of Validation.lean applies to it.
  exact all_classes_serial E hinit _ (valid_drop E h T.missed hv.1)

/-! ## The RED cases -/

/-- A small environment for the counterexamples: operands add one more than
themselves, `touches` always answers true, the log lives at keys 1000 and
up, and no key is content-addressed. -/
def demo : Env where
  -- The operator: an operand o adds o + 1.
  op := ⟨fun v o => v + o + 1⟩
  -- It never leaves 0: v + o + 1 is at least 1.
  apply_ne_zero := by intro v o; show v + o + 1 ≠ 0; omega
  -- The parts: every part of a value is the value itself, and every operand is said to touch.
  parts := ⟨fun v _ => v, fun _ _ => true⟩
  -- The touches law holds for lack of a case: no operand is ever said not to touch.
  touches_sound := by intro v o S h; cases h
  -- The log's head is key 1000.
  head := 1000
  -- Position p's entry is key 1001 + p.
  entry := fun p => 1001 + p
  -- The Log keys are 1000 and up.
  isLog := fun k => decide (1000 ≤ k)
  -- Key 1000 is one of them, by computing.
  isLog_head := by decide
  -- So is every entry key: 1001 + p is at least 1000.
  isLog_entry := by intro p; simp only [decide_eq_true_eq]; omega
  -- Different positions have different entry keys, by arithmetic.
  entry_inj := by intro p q h; omega
  -- No entry key is 1000, by arithmetic.
  entry_ne_head := by intro p; omega
  -- No key is content-addressed.
  isCA := fun _ => false
  -- So no key is both: there is no content-addressed key to speak of.
  ca_not_log := by intro k h; cases h
  -- Every key's bytes are 1 (unused).
  bytes := fun _ => 1
  -- Which is not 0.
  bytes_ne_zero := by intro k; omega
  -- Every key starts absent.
  init := fun _ => 0

/-- A decider that copies key 5, read without validation, into key 6, and a
writer of key 5 that commits after the decider's snapshot. -/
def decider : Txn := ⟨1, [.exempt], [], fun σ => [.put 6 (σ 5)], fun _ => []⟩
/-- The writer: key 5 set to 1. -/
def writer : Txn := ⟨0, [], [], fun _ => [.put 5 1], fun _ => []⟩

/-- **`exempt_decision_breaks_serial`.** A decision on an exempt read is not
serial: the decider copied 0, while run after the writer it copies 1. This
is why a caller must not decide on a Log read, a commutative stretch, or
parts it did not name (the RED configs `MC_DefraLevel_Red_DecideOnLogRead`
and `MC_DefraLevel_Red_PartsNamesOther`). -/
theorem exempt_decision_breaks_serial :
    -- The statement: in the engine's history key 6 ends at 0, and in the one-at-a-time run it ends
    -- at 1.
    run demo [decider, writer] 6 = 0 ∧ serial demo [decider, writer] 6 = 1 := by
  -- Two things to show.
  constructor
  -- The engine's history: unfold the two commits.
  · rw [run_cons, run_cons]
    -- Compute: the decider read key 5 before the writer's put, so it copied 0.
    simp [run, txnWrites, decider, writer, applyWrites, applyWrite, upd, demo]
  -- The one-at-a-time run: compute. The decider runs after the writer and copies 1.
  · simp [serial, txnWrites, decider, writer, applyWrites, applyWrite, upd, demo]

/-- **`promoted_read_skew`.** A pessimistic transaction that reads key 1 at
its snapshot and key 2 with get_for_update after a writer changed both reads
(0, 1), and neither state of the history shows that pair. If such a
transaction validated nothing, as a write-free optimistic one may, it would
commit a view of no point in time (`MC_DefraLevel_Red_WriteFreePessimistic`). -/
theorem promoted_read_skew :
    -- The statement: key 1 before the writer is 0; key 2 after it is 1; before the writer the pair
    -- is not (0, 1); after it the pair is not (0, 1) either.
    demo.init 1 = 0 ∧ applyWrites demo demo.init [.put 1 1, .put 2 1] 2 = 1 ∧
      ¬ (demo.init 1 = 0 ∧ demo.init 2 = 1) ∧
      ¬ (applyWrites demo demo.init [.put 1 1, .put 2 1] 1 = 0 ∧
         applyWrites demo demo.init [.put 1 1, .put 2 1] 2 = 1) := by
  -- Key 1 and key 2 before the writer, and after it.
  simp [applyWrites, applyWrite, upd, demo]


/-- The bank of the read-only anomaly (Fekete, O'Neil and O'Neil 2004), the
worked example the TLA+ model, the Rust tests and the docs share: checking
is key 1 and holds 30, savings is key 2 and holds 0 (absent). Built from
`demo`, the small environment above, where every key starts absent. -/
def bankStart : State := upd demo.init 1 30

/-- The fee the withdrawal charges on the balances it saw in `σ`: 1 when
their total is under 35, else 0. -/
def fee (σ : State) : Nat := if σ 1 + σ 2 < 35 then 1 else 0

/-- The withdrawal, decided on the state `σ` it read: checking, key 1, put
to its balance less 25 and the fee. -/
def withdrawal (σ : State) : List Write := [.put 1 (σ 1 - 25 - fee σ)]

/-- The deposit, decided on the state `σ` it read: savings, key 2, put 20
higher. -/
def deposit (σ : State) : List Write := [.put 2 (σ 2 + 20)]

/-- What the report reads: the state after the deposit alone, with the
withdrawal not yet published. -/
def afterDeposit : State := applyWrites demo bankStart (deposit bankStart)

/-- The state after the withdrawal alone, run first on the starting state. -/
def afterWithdrawal : State := applyWrites demo bankStart (withdrawal bankStart)

/-- **`read_only_anomaly` (E50).** The worked example, in numbers. The
withdrawal reads (30, 0), so it will charge the fee; the deposit commits
savings = 20; the report reads (30, 20); the withdrawal commits checking =
30 - 25 - 1 = 4, decided on what it read, so the history ends at (4, 20).

Run one at a time, deposit then withdrawal ends at checking 5 (no fee: the
withdrawal sees 50), not the history's state. Withdrawal then deposit ends
at the history's state, passing through (30, 0), (4, 0) and (4, 20), none of
which is the report's (30, 20). So a report that commits (30, 20) after the
withdrawal claims a sheet no order of the three gives.

The last line is why a report at RepeatableRead or Serializable is refused
there: its check of checking (`.full 1`, key 1 checked by sequence),
against the withdrawal's write, fails. `MC_Serializable_Red_ReadOnlyAnomaly`
plants the bug of skipping that check, and
`MC_Serializable_Red_ReadOnlyAnomalyPublishedOnly` the bug of counting only
published writes while the withdrawal is decided and not yet visible. -/
theorem read_only_anomaly :
    -- The report reads checking 30 and savings 20;
    (afterDeposit 1 = 30 ∧ afterDeposit 2 = 20) ∧
      -- the history (the deposit, then the withdrawal decided at the start) ends at (4, 20);
      (applyWrites demo afterDeposit (withdrawal bankStart) 1 = 4 ∧
        applyWrites demo afterDeposit (withdrawal bankStart) 2 = 20) ∧
      -- deposit then withdrawal, one at a time, leaves checking 5, not the history's 4;
      applyWrites demo afterDeposit (withdrawal afterDeposit) 1 = 5 ∧
      -- withdrawal then deposit starts at (30, 0), which is not the report's (30, 20),
      (bankStart 1 = 30 ∧ bankStart 2 = 0) ∧
      -- passes through (4, 0), which is not it either,
      (afterWithdrawal 1 = 4 ∧ afterWithdrawal 2 = 0) ∧
      -- and ends at the history's (4, 20), which is not it either;
      (applyWrites demo afterWithdrawal (deposit afterWithdrawal) 1 = 4 ∧
        applyWrites demo afterWithdrawal (deposit afterWithdrawal) 2 = 20) ∧
      -- and the report's check of checking, against the withdrawal's write, fails.
      ¬ Ok demo (.full 1) afterDeposit (withdrawal bankStart) := by
  -- Every part is a computation on small numbers: unfold the states, the writes and the check, and
  -- evaluate.
  simp [afterDeposit, afterWithdrawal, bankStart, deposit, withdrawal, fee, applyWrites,
    applyWrite, upd, demo, Ok, lastWhere, affects]

/-- **`blindMerge_across_replacement`: why the blind-merge rule refuses a newer
replacement.** With a put of key 7 between a merge's snapshot and its
commit, the merge placed at its commit lands on the put and counts, while
placed at its snapshot the put erases it: the two positions disagree, so a
merge that crossed a replacement has no single place in the history that
both its snapshot and its effect agree with
(`MC_DefraLevel_Red_MergeIgnoresReplacement`). -/
theorem blindMerge_across_replacement (σ : State) (o : Nat) :
    -- The statement: put then merge, and merge then put, leave different values on key 7.
    applyWrite demo (applyWrite demo σ (.put 7 5)) (.merge 7 o) 7 ≠
      applyWrite demo (applyWrite demo σ (.merge 7 o)) (.put 7 5) 7 := by
  -- At its commit it gives 5 + o + 1; at its snapshot the put leaves 5.
  simp [applyWrite, upd, demo]
  -- 5 + o + 1 is never 5, by arithmetic.
  omega

-- The end of the additions to Regolith.Validation.
end Regolith.Validation
