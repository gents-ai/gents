/-!
# MirrorPersist: OPFS mirror mode writes its batches back one at a time, in order

THE STORY. A database opened on a browser's main thread lives in memory
(`MirrorFs`, `src/env/opfs/mirror.rs`) and reaches OPFS only when the app
calls `OpfsEnv::persist` (`src/env/opfs/mod.rs:322`). One call is one batch:
it captures the dirty files and the names owed a removal, writes the files
whole (tables first, the MANIFEST last: `persist_rank`,
`src/env/persist_order.rs:17`), removes the owed names, and settles: each
written version counts as persisted, each removed name stops being owed.
Every browser call is a promise the batch awaits, and other code runs while
it waits: the engine, and another `persist` call.

Tiny example of what goes wrong without the lock (`persist_lock`,
mirror.rs:328). Batch A captures MANIFEST version 1 and starts writing it.
The engine appends an edit: version 2. Batch B captures version 2, writes
it, and the write lands. Then A's write lands: OPFS holds version 1 again,
older than what it held a moment before.

WHAT IS PROVED, for every size:

1. `serial_never_older` and `serial_monotone`: with the lock taken before
   the capture and held until the batch ends, no step of any run, of any
   length, lowers the MANIFEST version OPFS holds: any number of edits,
   batches and failures. `overlap_stores_older` is the RED: let a second
   batch capture while the first one's write is still to land (no lock, a
   batch dropped with its lock, or a capture before the lock), and five
   steps take OPFS from version 2 back to 1.
2. `failed_removal_stays_owed` and `failure_keeps_every_name`: a batch in
   which one removal failed with anything but NotFoundError ends before
   settle, so every name owed before it, the failed one included, is still
   owed after it, for any number of names. `all_gone_clears` is the other
   half: when every removal worked or found no entry, every captured name
   stops being owed. `swallow_loses_failed` is the RED of the code before
   #289, which counted the failure as done. `strict_owed_forever` is the
   RED the other way: count NotFoundError as a failure, and a name whose
   entry is already gone stays owed after every retry.
3. `cut_short_keeps_lower`: in a batch in persist order (no call after one
   of a higher rank), whatever prefix of it ran (a tab closed, or a call
   failed and the batch returned), every call of a lower rank than one that
   ran also ran. So `cut_short_keeps_tables`: once the MANIFEST is written,
   every table of the batch is; and `cut_short_keeps_writes`: once a
   removal ran, every write of the batch went through, the MANIFEST
   included. `batch_ordered` shows that the code's batch, its writes sorted
   by rank and then its removals, is in persist order.
   `manifest_first_not_ordered`, `manifest_first_loses_table` and
   `removal_first_loses_manifest` are the REDs.

BACKS `proofs/tla/MirrorPersist.tla`, which checks every interleaving of two
batches, the engine and the browser in a small run: `INV_ManifestMonotone`
there is `serial_never_older` here for runs of every length,
`INV_RemovalOwed` is `failed_removal_stays_owed`, `INV_ManifestNamesStored`
rests on `cut_short_keeps_tables` and `cut_short_keeps_writes`. Of the RED
configurations there, `NoLock`, `NoKeepAlive` and `CaptureFirst` are
`overlap_stores_older` here, `SwallowRemoval` is `swallow_loses_failed`,
`NotFoundFails` is `strict_owed_forever`, `ManifestFirst` is
`manifest_first_not_ordered` and `manifest_first_loses_table`, and
`DeleteFirst` and `DeleteAfterFailedWrite` are
`removal_first_loses_manifest`. `NoRetry`, a caller who stops calling, is a
run that ends, which only the TLA+ model's liveness check can show.

How to read the Lean: `def` defines a function or a property, `inductive`
lists the ways something can be built or can happen, `theorem` states a fact
and its proof follows `:= by` or `:=`. Lines starting with `--` are comments;
each proof step is commented.
-/

-- Everything below is named Regolith.MirrorPersist.<name>.
namespace Regolith.MirrorPersist

/-! ## 1. The MANIFEST version OPFS holds never goes down -/

/-- The part of the state the MANIFEST's story needs. -/
structure St where
  /-- The MANIFEST's version in the mirror (`Entry::version`, mirror.rs:93),
  raised by every edit the engine appends. -/
  ver : Nat
  /-- The MANIFEST version OPFS holds: what the next mount reads. -/
  disk : Nat
  /-- The versions captured by batches whose MANIFEST write has not landed
  yet, newest first. -/
  pending : List Nat

/-- One step of a run. `lock` says whether a batch needs the lock to capture
(`persist_batch`, mirror.rs:328-329): `true` is the code. With `false`, a
second batch captures while the first one's write is still to land, which
no lock, a batch dropped with its lock (no keep-alive) and a capture made
before the lock all allow. -/
inductive Step (lock : Bool) : St → St → Prop
  /-- The engine appends an edit to the MANIFEST: its version goes up by one. -/
  | install (v d : Nat) (p : List Nat) : Step lock ⟨v, d, p⟩ ⟨v + 1, d, p⟩
  /-- A batch captures the version now. With the lock, no other batch may
  hold a capture then. -/
  | capture (v d : Nat) (p : List Nat) (free : lock = true → p = []) :
      -- After the step: one more capture held, of the version now.
      Step lock ⟨v, d, p⟩ ⟨v, d, v :: p⟩
  /-- One held capture's write lands: OPFS holds that version, and its batch
  ends. -/
  | store (v d c : Nat) (p q : List Nat) : Step lock ⟨v, d, p ++ c :: q⟩ ⟨v, c, p ++ q⟩
  /-- A batch holding a capture ends with an error before its MANIFEST write
  landed: OPFS keeps what it held. -/
  | fail (v d c : Nat) (p q : List Nat) : Step lock ⟨v, d, p ++ c :: q⟩ ⟨v, d, p ++ q⟩

/-- Any number of steps, one after another. -/
inductive Steps (lock : Bool) : St → St → Prop
  /-- No step at all. -/
  | refl (s : St) : Steps lock s s
  /-- Some steps, then one more. -/
  | tail {s t u : St} : Steps lock s t → Step lock t u → Steps lock s u

/-- The start: the open wrote MANIFEST version 1 into the mirror, OPFS holds
none (version 0), and no batch holds a capture. -/
def init : St := ⟨1, 0, []⟩

/-- What the code keeps true at every step. -/
structure Inv (s : St) : Prop where
  /-- OPFS holds no version above the mirror's. -/
  disk_le : s.disk ≤ s.ver
  /-- At most one batch holds a capture: the lock. -/
  one : s.pending.length ≤ 1
  /-- A held capture lies between what OPFS holds and the mirror's version. -/
  between : ∀ c, c ∈ s.pending → s.disk ≤ c ∧ c ≤ s.ver

/-- The start keeps it. -/
theorem init_inv : Inv init :=
  -- 0 is at most 1, no capture is held, and so none lies anywhere.
  ⟨Nat.zero_le _, Nat.zero_le _, fun _ h => absurd h List.not_mem_nil⟩

/-- **Every step of the code keeps the invariant, and no step lowers the
version OPFS holds.** -/
theorem step_inv {s t : St}
    -- The invariant holds before the step,
    (hinv : Inv s)
    -- and the step is one the code takes, with the lock.
    (hstep : Step true s t) :
    -- Then it holds after, and OPFS's version did not go down.
    Inv t ∧ s.disk ≤ t.disk := by
  -- Look at which step it was.
  cases hstep with
  -- The engine appends an edit.
  | install v d p =>
    -- OPFS was at most the version.
    have hle : d ≤ v := hinv.disk_le
    -- At most one capture was held.
    have hone : p.length ≤ 1 := hinv.one
    -- Each held capture lay between.
    have hbet : ∀ c, c ∈ p → d ≤ c ∧ c ≤ v := hinv.between
    -- The version rose by one; OPFS and the captures stay. Two goals are left.
    refine ⟨⟨?_, hone, ?_⟩, Nat.le_refl d⟩
    -- First: OPFS is still at most the version.
    · show d ≤ v + 1
      -- It was at most one less.
      omega
    -- Second: a held capture still lies between.
    · intro c hc
      -- Its old bounds.
      have hb := hbet c hc
      -- The lower one stays, the upper one only loosens by one.
      exact ⟨hb.1, Nat.le_succ_of_le hb.2⟩
  -- A batch captures, the lock being free.
  | capture v d p hfree =>
    -- The lock was free: no capture was held.
    have hp : p = [] := hfree rfl
    -- Put the empty list for `p` everywhere.
    subst hp
    -- OPFS was at most the version.
    have hle : d ≤ v := hinv.disk_le
    -- One capture now, of the version; OPFS stays. One goal is left.
    refine ⟨⟨hle, Nat.le_refl 1, ?_⟩, Nat.le_refl d⟩
    -- Take a held capture `c`.
    intro c hc
    -- The only one held is the version now.
    have hcv : c = v := List.mem_singleton.mp hc
    -- Put one name for both.
    subst hcv
    -- OPFS is at most it, and it is at most itself.
    exact ⟨hle, Nat.le_refl _⟩
  -- A batch's MANIFEST write lands.
  | store v d c p q =>
    -- At most one capture was held,
    have hone : (p ++ c :: q).length ≤ 1 := hinv.one
    -- and that length is the lengths of `p` and `q` plus one.
    rw [List.length_append, List.length_cons] at hone
    -- The landing capture lay between OPFS and the version.
    have hc : d ≤ c ∧ c ≤ v := hinv.between c (by simp)
    -- After: OPFS holds `c`, at most the version and at least what OPFS
    -- held. Two goals are left.
    refine ⟨⟨hc.2, ?_, ?_⟩, hc.1⟩
    -- First: no capture is held now.
    · show (p ++ q).length ≤ 1
      -- The length of the two parts.
      rw [List.length_append]
      -- Both parts are empty.
      omega
    -- Second: so none lies anywhere.
    · intro c' hc'
      -- A capture held would make the list non-empty,
      have hpos : 0 < (p ++ q).length := List.length_pos_of_mem hc'
      -- and its length is the two parts' lengths,
      rw [List.length_append] at hpos
      -- which are both 0: a contradiction closes the goal.
      exact absurd hpos (by omega)
  -- A batch ends with an error.
  | fail v d c p q =>
    -- At most one capture was held,
    have hone : (p ++ c :: q).length ≤ 1 := hinv.one
    -- and that length is the lengths of `p` and `q` plus one.
    rw [List.length_append, List.length_cons] at hone
    -- OPFS was at most the version.
    have hle : d ≤ v := hinv.disk_le
    -- After: OPFS keeps what it held. Two goals are left.
    refine ⟨⟨hle, ?_, ?_⟩, Nat.le_refl d⟩
    -- First: no capture is held now.
    · show (p ++ q).length ≤ 1
      -- The length of the two parts.
      rw [List.length_append]
      -- Both parts are empty.
      omega
    -- Second: so none lies anywhere.
    · intro c' hc'
      -- A capture held would make the list non-empty,
      have hpos : 0 < (p ++ q).length := List.length_pos_of_mem hc'
      -- and its length is the two parts' lengths,
      rw [List.length_append] at hpos
      -- which are both 0: a contradiction closes the goal.
      exact absurd hpos (by omega)

/-- Any number of the code's steps keep the invariant. -/
theorem steps_inv {s t : St}
    -- Steps of the code from `s` to `t`.
    (h : Steps true s t) : Inv s → Inv t := by
  -- Walk the steps.
  induction h with
  -- No step: the same state.
  | refl => exact id
  -- One more step: the invariant carries over it.
  | tail _ hstep ih => exact fun hs => (step_inv (ih hs) hstep).1

/-- Along any steps of the code, OPFS's version only goes up. -/
theorem steps_mono {s t : St}
    -- Steps of the code from `s` to `t`.
    (h : Steps true s t) : Inv s → s.disk ≤ t.disk := by
  -- Walk the steps.
  induction h with
  -- No step: the same version.
  | refl => exact fun _ => Nat.le_refl _
  -- One more step: up to the state before it, then that step.
  | tail hst hstep ih =>
    -- The invariant holds before the last step, and the step keeps the version.
    exact fun hs => Nat.le_trans (ih hs) (step_inv (steps_inv hst hs) hstep).2

/-- The code's reachable states keep the invariant. -/
theorem reachable_inv {s : St}
    -- `s` is reached from the start by the code.
    (h : Steps true init s) : Inv s :=
  -- The start keeps it, and so does every step.
  steps_inv h init_inv

/-- **OPFS's MANIFEST never goes back.** In every run of the code, of any
length, no step lowers the MANIFEST version OPFS holds. -/
theorem serial_never_older {s t : St}
    -- `s` is reached by the code,
    (h : Steps true init s)
    -- and one more step of the code reaches `t`.
    (hstep : Step true s t) : s.disk ≤ t.disk :=
  -- The invariant holds at `s`, and the step keeps OPFS's version.
  (step_inv (reachable_inv h) hstep).2

/-- The same over any stretch of a run: a later state holds a version at
least as new. -/
theorem serial_monotone {s t : St}
    -- `s` is reached by the code,
    (h : Steps true init s)
    -- and more steps of the code reach `t`.
    (h' : Steps true s t) : s.disk ≤ t.disk :=
  -- The invariant holds at `s`, and every step after keeps the version.
  steps_mono h' (reachable_inv h)

/-- **RED.** Without the lock, a short run takes OPFS's MANIFEST from version
2 back to 1: batch A captures version 1, the engine appends an edit, batch
B captures version 2, B's write lands, and then A's. -/
theorem overlap_stores_older :
    -- Some run reaches a state where one more step lowers OPFS's version.
    ∃ s t, Steps false init s ∧ Step false s t ∧ t.disk < s.disk := by
  -- The state before A's write lands and after it; then 1 < 2. One goal is left.
  refine ⟨⟨2, 2, [1]⟩, ⟨2, 1, []⟩, ?_, .store 2 2 1 [] [], by decide⟩
  -- The run to the state before: four steps from the start.
  exact .tail (.tail (.tail (.tail (.refl _)
    -- A captures 1 (no lock asked for);
    (.capture 1 0 [] (fun _ => rfl)))
    -- the engine appends an edit;
    (.install 1 0 [1]))
    -- B captures 2 while A's capture is still held;
    (.capture 2 0 [1] (fun h => absurd h (by decide))))
    -- and B's write lands.
    (.store 2 0 2 [] [1])

/-! ## 2. A failed removal stays owed -/

/-- What `removeEntry` can come back with (mirror.rs:344). -/
inductive Outcome where
  /-- The entry was removed. -/
  | removed
  /-- There was no entry: `NotFoundError`. -/
  | notFound
  /-- Any other failure, like `NotAllowedError`. -/
  | failed

/-- Whether the code counts an outcome as the name being gone
(mirror.rs:344-349): removed, or no entry to remove. -/
def gone : Outcome → Bool
  -- Removed: gone.
  | .removed => true
  -- No entry: gone already.
  | .notFound => true
  -- Any other failure: still there, and the call returns the error.
  | .failed => false

/-- The code before #289 (`let _ = remove_entry(..)`): every outcome counted
as gone. -/
def goneSwallow (_ : Outcome) : Bool := true

/-- The RED `NotFoundFails`: NotFoundError counted as a failure. -/
def goneStrict : Outcome → Bool
  -- Removed: gone.
  | .removed => true
  -- No entry: wrongly counted as a failure.
  | .notFound => false
  -- Any other failure: a failure.
  | .failed => false

-- The names: any type.
variable {α : Type}

/-- Whether every removal of the batch, made in order, counts as gone. The
loop stops at the first that does not and the call returns its error
(mirror.rs:341-350). -/
def allGone (g : Outcome → Bool) (out : α → Outcome) : List α → Bool
  -- No removal left: the batch gets to settle.
  | [] => true
  -- This one counts as gone, and so do the rest.
  | n :: ns => g (out n) && allGone g out ns

/-- Whether name `n` is owed a removal after a batch that captured the owed
names `ns`, each removal coming back with `out`: settle (mirror.rs:352,
309) forgets the captured names only if the batch got there. -/
def owedAfter (g : Outcome → Bool) (out : α → Outcome) (owed : α → Prop) (ns : List α)
    -- The name asked about.
    (n : α) : Prop :=
  -- Settle ran: owed before and not captured. Otherwise: owed before.
  if allGone g out ns = true then owed n ∧ n ∉ ns else owed n

/-- One removal that does not count as gone stops the batch before settle. -/
theorem allGone_false (g : Outcome → Bool) (out : α → Outcome) (n : α) :
    -- For any list of names holding `n`, whose removal does not count as gone.
    ∀ ns : List α, n ∈ ns → g (out n) = false → allGone g out ns = false := by
  -- Take the list.
  intro ns
  -- Induction on it.
  induction ns with
  -- The empty list holds nothing.
  | nil =>
    -- `n` would be in the empty list.
    intro hn
    -- It is not: the goal follows from the contradiction.
    exact absurd hn List.not_mem_nil
  -- A first name `m`, then the rest `ms`.
  | cons m ms ih =>
    -- `n` is in the list, and its removal does not count as gone.
    intro hn hg
    -- `n` is the first name, or among the rest.
    rcases List.mem_cons.mp hn with he | hm
    -- The first: put one name for both.
    · subst he
      -- Its removal is not gone, so neither is the whole batch's.
      simp [allGone, hg]
    -- Among the rest: the rest is not all gone, by induction.
    · simp [allGone, ih hm hg]

/-- Every removal counted as gone lets the batch reach settle. -/
theorem allGone_true (g : Outcome → Bool) (out : α → Outcome) :
    -- For any list of names whose every removal counts as gone.
    ∀ ns : List α, (∀ m, m ∈ ns → g (out m) = true) → allGone g out ns = true := by
  -- Take the list.
  intro ns
  -- Induction on it.
  induction ns with
  -- No removal: the batch reaches settle.
  | nil =>
    -- Nothing to assume; the empty batch is all gone by definition.
    intro _
    -- Unfold: `true = true`.
    rfl
  -- A first name `m`, then the rest `ms`.
  | cons m ms ih =>
    -- Every removal counts as gone.
    intro hall
    -- The first one does.
    have hm : g (out m) = true := hall m (by simp)
    -- The rest do, by induction.
    have hms : allGone g out ms = true := ih (fun k hk => hall k (List.mem_cons_of_mem _ hk))
    -- Both are true, so the whole batch is.
    simp [allGone, hm, hms]

/-- **A failure keeps every name owed.** If removing one of the batch's names
failed with anything but NotFoundError, every name owed before the batch is
still owed after it, for any list of names: the next call tries them all
again. -/
theorem failure_keeps_every_name (out : α → Outcome) (owed : α → Prop) (ns : List α) (n : α)
    -- `n` is one of the batch's names, and removing it failed.
    (hn : n ∈ ns) (hfail : out n = .failed)
    -- Any name `k` owed before the batch,
    (k : α) (hk : owed k) :
    -- is still owed after it.
    owedAfter gone out owed ns k := by
  -- The batch did not reach settle: one removal did not count as gone.
  have hf : allGone gone out ns = false := allGone_false gone out n ns hn (by simp [hfail, gone])
  -- So the test settle runs under is false.
  have hnot : ¬ (allGone gone out ns = true) := by simp [hf]
  -- Look at what is owed after.
  unfold owedAfter
  -- The batch returned before settle: what was owed stays owed.
  rw [ite_eq_right hnot]
  -- `k` was owed.
  exact hk

/-- **A failed removal stays owed.** The name whose removal failed with
anything but NotFoundError is still owed after the batch. -/
theorem failed_removal_stays_owed (out : α → Outcome) (owed : α → Prop) (ns : List α)
    -- `n` is one of the batch's names, owed before it,
    (n : α) (hn : n ∈ ns) (howed : owed n)
    -- and removing it failed with something other than NotFoundError.
    (hfail : out n = .failed) :
    -- Then it is still owed after the batch.
    owedAfter gone out owed ns n :=
  -- The theorem above, for `k = n`.
  failure_keeps_every_name out owed ns n hn hfail n howed

/-- A removal that worked or found no entry counts as gone. -/
theorem gone_of_ne_failed (o : Outcome)
    -- The outcome is not a failure.
    (h : o ≠ .failed) : gone o = true := by
  -- Look at the outcome.
  cases o with
  -- Removed: gone, by definition.
  | removed => rfl
  -- No entry: gone, by definition.
  | notFound => rfl
  -- A failure: ruled out.
  | failed => exact absurd rfl h

/-- **No failure clears every captured name.** When every removal of the
batch worked or found no entry, the batch settles and every name it
captured is owed no more. -/
theorem all_gone_clears (out : α → Outcome) (owed : α → Prop) (ns : List α)
    -- No removal of the batch failed.
    (hall : ∀ m, m ∈ ns → out m ≠ .failed)
    -- A name the batch captured.
    (n : α) (hn : n ∈ ns) :
    -- It is not owed after the batch.
    ¬ owedAfter gone out owed ns n := by
  -- Every removal counts as gone.
  have ht : allGone gone out ns = true :=
    -- Each one, by the lemma above.
    allGone_true gone out ns (fun m hm => gone_of_ne_failed (out m) (hall m hm))
  -- Look at what is owed after.
  unfold owedAfter
  -- Settle ran: owed means owed before and not captured.
  rw [ite_eq_left ht]
  -- Suppose `n` were still owed.
  intro h
  -- It was captured: a contradiction.
  exact h.2 hn

/-- NotFoundError counts as removed: a name whose entry is already gone does
not fail the batch. -/
theorem not_found_is_gone : gone .notFound = true :=
  -- By definition.
  rfl

/-- **RED.** The code before #289 counted every outcome as gone: a batch whose
one removal failed settles and forgets the name. -/
theorem swallow_loses_failed :
    -- Name 1 owed and captured; its removal failed; it is not owed after.
    ¬ owedAfter goneSwallow (fun (_ : Nat) => Outcome.failed) (fun n => n = 1) [1] 1 := by
  -- The swallowing batch counts the failure as gone, so it reaches settle.
  have ht : allGone goneSwallow (fun (_ : Nat) => Outcome.failed) [1] = true := rfl
  -- Look at what is owed after.
  unfold owedAfter
  -- Settle ran: owed means owed before and not captured.
  rw [ite_eq_left ht]
  -- Suppose name 1 were still owed.
  intro h
  -- It was captured: a contradiction.
  exact h.2 (by simp)

/-- **RED `NotFoundFails`.** Count NotFoundError as a failure, and a name whose
entry is already gone keeps the batch from settling, so it stays owed. -/
theorem strict_keeps_absent (out : α → Outcome) (owed : α → Prop) (ns : List α) (n : α)
    -- `n` is captured and owed, and OPFS has no entry for it.
    (hn : n ∈ ns) (howed : owed n) (habsent : out n = .notFound) :
    -- It is still owed after the batch.
    owedAfter goneStrict out owed ns n := by
  -- The batch did not reach settle.
  have hf : allGone goneStrict out ns = false :=
    -- Its removal of `n` did not count as gone.
    allGone_false goneStrict out n ns hn (by simp [habsent, goneStrict])
  -- So the test settle runs under is false.
  have hnot : ¬ (allGone goneStrict out ns = true) := by simp [hf]
  -- Look at what is owed after.
  unfold owedAfter
  -- The batch returned before settle.
  rw [ite_eq_right hnot]
  -- `n` was owed.
  exact howed

/-- What is owed after `k` batches, each capturing the same names `ns` and
getting the same outcomes `out`: an entry that is gone stays gone, so its
removal finds no entry every time. -/
def afterRetries (g : Outcome → Bool) (out : α → Outcome) (owed : α → Prop) (ns : List α) :
    -- For a number of batches, whether a name is owed.
    Nat → α → Prop
  -- Before any batch: what was owed.
  | 0 => owed
  -- After one more batch.
  | k + 1 => owedAfter g out (afterRetries g out owed ns k) ns

/-- **RED `NotFoundFails`, for every retry.** Counting NotFoundError as a
failure, a name whose entry is already gone is owed after any number of
retries: every call fails there, and the work never ends. -/
theorem strict_owed_forever (out : α → Outcome) (owed : α → Prop) (ns : List α) (n : α)
    -- `n` is captured and owed, and OPFS has no entry for it.
    (hn : n ∈ ns) (howed : owed n) (habsent : out n = .notFound) :
    -- Owed after every number of batches.
    ∀ k, afterRetries goneStrict out owed ns k n := by
  -- Take the number of batches.
  intro k
  -- Induction on it.
  induction k with
  -- None: owed, as before.
  | zero => exact howed
  -- One more: owed before it, so owed after it.
  | succ k ih => exact strict_keeps_absent out (afterRetries goneStrict out owed ns k) ns n hn ih habsent

/-! ## 3. The order: tables, then the MANIFEST, then the removals -/

/-- What a call of a batch does, as the order sees it. -/
inductive Kind where
  /-- Write a table (`.sst`). -/
  | table
  /-- Write any other file: a log, MANIFEST.tmp, LOCK. -/
  | other
  /-- Write the MANIFEST. -/
  | manifest
  /-- Remove an owed name. -/
  | removal
  -- Two kinds can be compared, so `decide` can check small cases.
  deriving DecidableEq

/-- Where a call goes in a batch, lower first: `persist_rank`
(persist_order.rs:17-25) for the writes, and the removals after every write
(the loop at mirror.rs:341 after the loop at 337). -/
def rank : Kind → Nat
  -- Tables first.
  | .table => 0
  -- Then every other file.
  | .other => 1
  -- Then the MANIFEST.
  | .manifest => 2
  -- Then the removals.
  | .removal => 3

/-- A batch in persist order: no call comes after one of a higher rank. -/
inductive Ordered (kind : α → Kind) : List α → Prop
  /-- An empty batch. -/
  | nil : Ordered kind []
  /-- A first call that no later call outranks from below, -/
  | cons (a : α) (l : List α) (first : ∀ b, b ∈ l → rank (kind a) ≤ rank (kind b))
      -- and the calls after it in order too.
      (rest : Ordered kind l) : Ordered kind (a :: l)

/-- **A batch cut short keeps every lower rank.** In a batch in persist order,
whatever prefix of it ran, if a call `x` ran then every call `y` of the batch
with a lower rank ran too, for every batch and every prefix. -/
theorem cut_short_keeps_lower (kind : α → Kind) :
    -- For every batch, prefix length, and pair of calls:
    ∀ (l : List α) (k : Nat) (x y : α), Ordered kind l → x ∈ l.take k → y ∈ l →
      rank (kind y) < rank (kind x) → y ∈ l.take k := by
  -- Take the batch.
  intro l
  -- Induction on it.
  induction l with
  -- An empty batch holds no call `y`.
  | nil =>
    -- Name the assumptions; only `y`'s membership matters.
    intro _ _ _ _ _ hy _
    -- `y` would be in the empty batch: a contradiction closes the goal.
    exact absurd hy List.not_mem_nil
  -- A first call `a`, then the rest `l`.
  | cons a l ih =>
    -- Name everything.
    intro k x y hord hx hy hlt
    -- The order gives: nothing after `a` outranks it from below, and the rest is in order.
    cases hord with
    -- That one way.
    | cons _ _ hfirst hrest =>
      -- Look at the prefix length.
      cases k with
      -- A prefix of length 0 holds no call `x`.
      | zero =>
        -- `x` would be in the empty prefix: a contradiction closes the goal.
        exact absurd hx List.not_mem_nil
      -- A prefix of length k + 1: `a`, then the first k of the rest.
      | succ k =>
        -- So `x` is `a` or in the first k of the rest.
        have hx' : x ∈ a :: l.take k := hx
        -- And the goal is that `y` is `a` or in the first k of the rest.
        show y ∈ a :: l.take k
        -- `y` is `a`, or in the rest.
        rcases List.mem_cons.mp hy with hya | hyl
        -- `y` is `a`: the prefix starts with it.
        · subst hya
          -- The first element is in the list.
          simp
        -- `y` is in the rest. Is `x` the first call?
        · rcases List.mem_cons.mp hx' with hxa | hxl
          -- `x` is `a`: put one name for both.
          · subst hxa
            -- `y` comes after it, so its rank is at least `x`'s,
            have hle := hfirst y hyl
            -- yet it is lower: a contradiction closes the goal.
            exact absurd hlt (by omega)
          -- `x` is in the rest's prefix: by induction `y` is too.
          · exact List.mem_cons_of_mem _ (ih k x y hrest hxl hyl hlt)

/-- A write ranks below a removal. -/
theorem rank_lt_removal {a b : Kind}
    -- `a` is not a removal and `b` is.
    (ha : a ≠ .removal) (hb : b = .removal) : rank a < rank b := by
  -- Put `removal` for `b`.
  subst hb
  -- Look at `a`.
  cases a with
  -- A table: 0 < 3.
  | table => decide
  -- Another file: 1 < 3.
  | other => decide
  -- The MANIFEST: 2 < 3.
  | manifest => decide
  -- A removal: ruled out.
  | removal => exact absurd rfl ha

/-- **The MANIFEST never goes without its tables.** Whatever prefix of a batch
in persist order ran, if it wrote the MANIFEST it wrote every table of the
batch. -/
theorem cut_short_keeps_tables (kind : α → Kind) (l : List α) (k : Nat) (m t : α)
    -- The batch is in persist order,
    (hord : Ordered kind l)
    -- the prefix that ran wrote the MANIFEST `m`,
    (hm : m ∈ l.take k) (hmk : kind m = .manifest)
    -- and `t` is a table of the batch.
    (ht : t ∈ l) (htk : kind t = .table) : t ∈ l.take k :=
  -- A table ranks below the MANIFEST.
  cut_short_keeps_lower kind l k m t hord hm ht (by simp [htk, hmk, rank])

/-- **No removal before every write.** Whatever prefix of a batch in persist
order ran, if it made a removal it made every write of the batch, the
MANIFEST included. -/
theorem cut_short_keeps_writes (kind : α → Kind) (l : List α) (k : Nat) (r w : α)
    -- The batch is in persist order,
    (hord : Ordered kind l)
    -- the prefix that ran made the removal `r`,
    (hr : r ∈ l.take k) (hrk : kind r = .removal)
    -- and `w` is a write of the batch.
    (hw : w ∈ l) (hwk : kind w ≠ .removal) : w ∈ l.take k :=
  -- A write ranks below a removal.
  cut_short_keeps_lower kind l k r w hord hr hw (rank_lt_removal hwk hrk)

/-- A list of removals only is in persist order. -/
theorem removals_ordered (kind : α → Kind) :
    -- For every list whose every call is a removal.
    ∀ rs : List α, (∀ r, r ∈ rs → kind r = .removal) → Ordered kind rs := by
  -- Take the list.
  intro rs
  -- Induction on it.
  induction rs with
  -- The empty list is in order.
  | nil =>
    -- Nothing to assume.
    intro _
    -- The empty batch.
    exact .nil
  -- A first removal `r`, then the rest.
  | cons r rs ih =>
    -- Every call is a removal.
    intro hall
    -- The first is.
    have hr : kind r = .removal := hall r (by simp)
    -- So are the rest.
    have hrest : ∀ b, b ∈ rs → kind b = .removal := fun b hb => hall b (List.mem_cons_of_mem _ hb)
    -- Same rank as everything after it, and the rest in order by induction.
    exact .cons r rs (fun b hb => Nat.le_of_eq (by rw [hr, hrest b hb])) (ih hrest)

/-- **The code's batch is in persist order.** Its writes sorted by rank
(`writes.sort_by_key(persist_rank)`, mirror.rs:295: sorted is in order),
none of them a removal, then its removals (the loop at 341), make a batch in
persist order. -/
theorem batch_ordered (kind : α → Kind) (rs : List α)
    -- The removals.
    (hrs : ∀ r, r ∈ rs → kind r = .removal) :
    -- For every list of writes in order, none a removal:
    ∀ ws : List α, Ordered kind ws → (∀ w, w ∈ ws → kind w ≠ .removal) →
      Ordered kind (ws ++ rs) := by
  -- Take the writes.
  intro ws
  -- Induction on them.
  induction ws with
  -- No writes: the batch is the removals.
  | nil =>
    -- Nothing else to assume.
    intro _ _
    -- The removals are in order.
    exact removals_ordered kind rs hrs
  -- A first write `w`, then the rest.
  | cons w ws ih =>
    -- The writes are in order and none is a removal.
    intro hord hno
    -- In order: nothing after `w` outranks it from below, and the rest in order.
    cases hord with
    -- That one way.
    | cons _ _ hfirst hrest =>
      -- `w` first, then the rest of the writes and the removals, in order by induction.
      refine .cons w (ws ++ rs) ?_ (ih hrest (fun v hv => hno v (List.mem_cons_of_mem _ hv)))
      -- Take a call `b` after `w`.
      intro b hb
      -- It is a write or a removal.
      rcases List.mem_append.mp hb with hbw | hbr
      -- A write: the writes' order.
      · exact hfirst b hbw
      -- A removal: it ranks above every write.
      · exact Nat.le_of_lt (rank_lt_removal (hno w (by simp)) (hrs b hbr))

/-- **RED `ManifestFirst`.** A batch that writes the MANIFEST before its table
is not in persist order. -/
theorem manifest_first_not_ordered : ¬ Ordered id [Kind.manifest, Kind.table] := by
  -- Suppose it were.
  intro h
  -- Then nothing after the MANIFEST would outrank it from below.
  cases h with
  -- That one way.
  | cons _ _ hfirst _ =>
    -- But the table after it ranks 0, below the MANIFEST's 2.
    exact absurd (hfirst Kind.table (by simp)) (by decide)

/-- **RED `ManifestFirst`, cut short.** That batch, cut short after one call,
has written the MANIFEST and not the table it names. -/
theorem manifest_first_loses_table :
    -- The MANIFEST ran, the table is in the batch, and the table did not run.
    Kind.manifest ∈ [Kind.manifest, Kind.table].take 1 ∧
      Kind.table ∈ [Kind.manifest, Kind.table] ∧
      Kind.table ∉ [Kind.manifest, Kind.table].take 1 := by
  -- Small and concrete: compute it.
  decide

/-- **RED `DeleteFirst` and `DeleteAfterFailedWrite`.** A batch whose removal
ran while its MANIFEST write did not: removals first, or removals after a
failed write. The removal ran, the MANIFEST that stops naming the removed
table did not. -/
theorem removal_first_loses_manifest :
    -- The removal ran and the MANIFEST did not.
    Kind.removal ∈ [Kind.removal, Kind.manifest].take 1 ∧
      Kind.manifest ∉ [Kind.removal, Kind.manifest].take 1 := by
  -- Small and concrete: compute it.
  decide

-- End of the namespace opened at the top.
end Regolith.MirrorPersist
