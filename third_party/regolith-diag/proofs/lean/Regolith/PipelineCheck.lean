/-!
# PipelineCheck: the check a slot runs when it is decided

## The story

Two threads commit at the same moment. Both read key 1 at snapshot 0, both
write key 1. If they commit one after the other, the second one notices that
key 1 changed after its snapshot and aborts, so the first one's write is not
lost.

The commit pipeline (`src/engine/commit`) gives every commit a slot of a
ring and decides the slots strictly in ring order. A slot's writes are in the
memtable before the next slot is decided (`AppliedBeforePassed` in
`proofs/tla/CommitPipeline.tla`), so the check of a slot reads a store that
already holds every write decided before it. A plain write (`Db::put`, which
is never checked) lands the same way, so a transaction behind it sees it too.

This file proves, for every number of slots and every read and write set:

1. `landed_write_is_seen`, `no_lost_update`, `later_slot_aborts` (TLA+
   `NoLostUpdate`): a write that landed is in the store every later slot is
   checked against, so a later transaction that touched its key from an
   older snapshot aborts, whatever slots stand between them and after them.
2. `abort_has_cause` (`AbortsHaveCause`): a slot aborts only when it is a
   transaction and the store holds a newer version of a key it touched.
3. `conflict_stays`: a conflict found among some versions is still a
   conflict once more versions land, which is why a transaction whose early
   check fails aborts at once, without taking a slot.
4. `early_check_split`, `early_check_cover`: the check is split in two
   (`src/engine/commit/early.rs`). Before it takes a slot a transaction
   checks itself, on its own thread, against the versions up to a horizon `h`
   it sampled, and whichever thread decides its slot then looks only above
   `h`. The two halves decide as one check of every version.
   `store_below_ceiling`, `ceiling_skips_the_check_above`,
   `skip_on_ceiling_is_one_check`: no version is newer than the last
   sequence handed out (the ceiling), so when that is at or below `h` the
   look above `h` finds nothing and the decision skips it; RED
   `skip_on_published_loses_an_update` guards the skip with the published
   horizon instead and loses an update (`MC_CommitPipeline_Red_SkipOnPublished`).
5. The RED case as a concrete counterexample:
   `published_only_loses_an_update`, a slot checked against a view that lacks
   an earlier slot's writes (`MC_CommitPipeline_Red_ValidatePublished`), and
   beside it `plain_write_is_seen`.

The sequences the slots take are `Regolith/Pipeline.lean`; who may take
which step is `Regolith/PipelineHelping.lean`. TLC checks the interleavings
of a few threads around these laws; the theorems here hold for every size.

## How to read the Lean

`def` defines a function or a property. `theorem` states a fact; its proof
follows `:= by` as a list of tactic steps, each of which changes the goal (what
is left to prove). Every line carries a `--` comment saying what it does.
-/

-- Everything below is named Regolith.PipelineCheck.<name>.
namespace Regolith.PipelineCheck

/-! ## The objects -/

/-
Keys and sequence numbers are plain natural numbers (`Nat`). A version is a
pair `(key, sequence)`: "key k was written at sequence s".
-/

/-- One commit as its slot holds it (`TxnRequest` or a plain `WriteRequest`
in `src/engine/commit`). -/
structure Txn where
  -- The sequence its snapshot reads at (`Transaction::snapshot_seq`).
  snap : Nat
  -- The keys it read or writes: a newer version of any of them aborts it.
  touched : List Nat
  -- The keys it writes.
  writes : List Nat
  -- A plain write: never checked, always lands.
  plain : Bool

/-- `conflicts vs t`: among the versions `vs`, one is of a key `t` touched,
at a sequence `t`'s snapshot did not see. This is the commit check. -/
def conflicts (vs : List (Nat × Nat)) (t : Txn) : Bool :=
  -- Some version `v` is of a touched key and newer than the snapshot.
  vs.any fun v => t.touched.contains v.1 && decide (t.snap < v.2)

/-- Whether a slot lands when it is decided against `store`: a plain write
always does, a transaction when nothing in the store conflicts with it. -/
def lands (store : List (Nat × Nat)) (t : Txn) : Bool :=
  -- Plain, or free of conflicts.
  t.plain || !conflicts store t

/-- The verdicts of the slots `ts`, decided in ring order: each is checked
against every version landed before it, earlier slots included, and a slot
that lands puts its writes in the store at the next sequence before the next
slot is checked. `true` means it committed, `false` that it aborted. `s` is
the last sequence handed out. -/
def decided : List (Nat × Nat) → Nat → List Txn → List Bool
  -- No slot left: no verdict.
  | _, _, [] => []
  -- The next slot `t`, then the `rest`.
  | store, s, t :: rest =>
    -- It lands: its writes go in at `s + 1`, ahead of the next slot.
    if lands store t then
      -- Its verdict is true; the rest are checked against the bigger store.
      true :: decided (t.writes.map (fun k => (k, s + 1)) ++ store) (s + 1) rest
    -- It aborts and writes nothing: the rest are checked against the same
    -- store.
    else false :: decided store s rest

/-- The store and the last sequence once the slots `ts` are decided: what
the slot after them is checked against. -/
def after : List (Nat × Nat) → Nat → List Txn → List (Nat × Nat) × Nat
  -- No slot left: the store and the sequence as they are.
  | store, s, [] => (store, s)
  -- The next slot `t`, then the `rest`.
  | store, s, t :: rest =>
    -- It lands: its writes join the store at `s + 1`.
    if lands store t then after (t.writes.map (fun k => (k, s + 1)) ++ store) (s + 1) rest
    -- It aborts: nothing changes.
    else after store s rest

/-! ## A landed write is seen by every later slot -/

/-- One step of `after` for a slot that lands. -/
theorem after_cons_lands (store : List (Nat × Nat)) (s : Nat) (t : Txn) (rest : List Txn)
    -- The slot lands.
    (hl : lands store t = true) :
    -- Its writes join the store, and the rest go on from there.
    after store s (t :: rest) =
      after (t.writes.map (fun k => (k, s + 1)) ++ store) (s + 1) rest := by
  -- Unfold one step and take the landing branch.
  simp [after, hl]

/-- One step of `after` for a slot that aborts. -/
theorem after_cons_aborts (store : List (Nat × Nat)) (s : Nat) (t : Txn) (rest : List Txn)
    -- The slot aborts.
    (hl : lands store t = false) :
    -- The rest go on from the same store and sequence.
    after store s (t :: rest) = after store s rest := by
  -- Unfold one step and take the aborting branch.
  simp [after, hl]

/-- **The store only grows.** A version in the store is still in it after
any number of slots are decided: deciding adds versions and removes none. -/
theorem store_grows (ts : List Txn) :
    -- For every store, sequence and version in the store.
    ∀ (store : List (Nat × Nat)) (s : Nat) (v : Nat × Nat),
      -- The version is still there afterwards.
      v ∈ store → v ∈ (after store s ts).1 := by
  -- By induction on the slots, for every store and sequence.
  induction ts with
  -- Case: no slot.
  | nil =>
    -- Take the store, the sequence, the version and where it is.
    intro store s v hv
    -- Nothing was decided: the store is the same.
    simpa [after] using hv
  -- Case: a first slot and then the rest, the claim known for the rest (ih).
  | cons t rest ih =>
    -- Take the store, the sequence, the version and where it is.
    intro store s v hv
    -- Does the first slot land?
    by_cases hl : lands store t = true
    -- It lands: the version is in the bigger store, and stays by induction.
    · rw [after_cons_lands store s t rest hl]
      -- The old store is the back part of the bigger one.
      exact ih _ _ v (List.mem_append_right _ hv)
    -- It aborts: the store is unchanged, and the version stays by induction.
    · simp only [Bool.not_eq_true] at hl
      -- Take the aborting step.
      rw [after_cons_aborts store s t rest hl]
      -- The rest keep the version.
      exact ih _ _ v hv

/-- **A landed write is seen.** A slot that lands at sequence `s + 1` and
writes key `k` leaves the version `(k, s + 1)` in the store every later slot
is checked against, however many slots `mid` are decided after it. -/
theorem landed_write_is_seen (store : List (Nat × Nat)) (s : Nat) (t : Txn) (mid : List Txn)
    -- The key in question.
    (k : Nat)
    -- The slot lands.
    (hl : lands store t = true)
    -- It writes key `k`.
    (hk : k ∈ t.writes) :
    -- Its version of `k` is in the store after it and after `mid`.
    (k, s + 1) ∈ (after store s (t :: mid)).1 := by
  -- The slot's own step puts its writes in front of the store.
  rw [after_cons_lands store s t mid hl]
  -- The version is among those writes, and the store only grows after that.
  exact store_grows mid _ _ _ (List.mem_append_left _ (List.mem_map.mpr ⟨k, hk, rfl⟩))

/-- **A conflict stays a conflict.** A conflict found among some versions is
still a conflict once more versions land: versions are only ever added, so
the version that conflicted is still there. -/
theorem conflict_stays {xs ys : List (Nat × Nat)} {t : Txn}
    -- Every version seen first is still there later.
    (hsub : ∀ v ∈ xs, v ∈ ys)
    -- The first check found a conflict.
    (hc : conflicts xs t = true) :
    -- The later check finds one too.
    conflicts ys t = true := by
  -- Unfold into "there is a version such that ...".
  simp only [conflicts, List.any_eq_true] at hc ⊢
  -- Take the conflicting version.
  obtain ⟨v, hv, hk⟩ := hc
  -- It is still there, and still conflicts.
  exact ⟨v, hsub v hv, hk⟩

/-- **No lost update.** A slot `t` lands and writes key `k`. A transaction
`u` whose snapshot is at or below `s`, the last sequence before `t`'s, touched
`k`. Then `u` aborts when it is decided after `t`, whatever slots `mid` are
decided between them: it cannot commit over `t`'s write. -/
theorem no_lost_update (store : List (Nat × Nat)) (s : Nat) (t u : Txn) (mid : List Txn)
    -- The key both slots meet on.
    (k : Nat)
    -- The earlier slot lands.
    (hl : lands store t = true)
    -- It writes key `k`.
    (hw : k ∈ t.writes)
    -- The later slot read or writes `k`.
    (ht : k ∈ u.touched)
    -- The later slot is a transaction, not a plain write.
    (hp : u.plain = false)
    -- Its snapshot did not see the earlier slot's write.
    (hsnap : u.snap ≤ s) :
    -- Checked against the store after `t` and `mid`, it does not land.
    lands (after store s (t :: mid)).1 u = false := by
  -- The earlier slot's version of `k` is in that store.
  have hin := landed_write_is_seen store s t mid k hl hw
  -- So the check finds a conflict.
  have hc : conflicts (after store s (t :: mid)).1 u = true := by
    -- Unfold into "there is a version such that ...".
    simp only [conflicts, List.any_eq_true, Bool.and_eq_true, List.contains_iff_mem,
      decide_eq_true_eq]
    -- The witness is `(k, s + 1)`: in the store, and of a touched key.
    refine ⟨(k, s + 1), hin, ht, ?_⟩
    -- What is left: the snapshot is below `s + 1`.
    show u.snap < s + 1
    -- It is at or below `s`.
    omega
  -- A transaction with a conflict does not land.
  simp [lands, hp, hc]

/-- Deciding a list of slots followed by another: the first list's verdicts,
then the second's, checked against the store and the sequence the first list
left. A slot's verdict depends only on the slots before it. -/
theorem decided_append (ts : List Txn) :
    -- For every store, sequence and second list.
    ∀ (store : List (Nat × Nat)) (s : Nat) (us : List Txn),
      -- The verdicts of the whole are those of the parts.
      decided store s (ts ++ us) =
        decided store s ts ++ decided (after store s ts).1 (after store s ts).2 us := by
  -- By induction on the first list, for every store and sequence.
  induction ts with
  -- Case: no slot in front.
  | nil =>
    -- Take the store, the sequence and the second list.
    intro store s us
    -- Nothing was decided before the second list.
    simp [decided, after]
  -- Case: a first slot and then the rest, the claim known for the rest (ih).
  | cons t rest ih =>
    -- Take the store, the sequence and the second list.
    intro store s us
    -- Does the first slot land?
    by_cases hl : lands store t = true
    -- It lands on both sides alike; the rest follow by induction.
    · simp [decided, after, hl, ih]
    -- It aborts on both sides alike; the rest follow by induction.
    · simp [decided, after, hl, ih]

/-- **The later slot's verdict is an abort, in any ring.** With the slots
`mid` between them and the slots `post` after them, the ring's verdicts are
those of `t` and `mid`, then `false` for `u`, then those of `post`: no
arrangement of other commits lets `u` commit over `t`'s write. -/
theorem later_slot_aborts (store : List (Nat × Nat)) (s : Nat) (t u : Txn)
    -- The slots between the two and after them, and the key both meet on.
    (mid post : List Txn) (k : Nat)
    -- The earlier slot lands.
    (hl : lands store t = true)
    -- It writes key `k`.
    (hw : k ∈ t.writes)
    -- The later slot read or writes `k`.
    (ht : k ∈ u.touched)
    -- The later slot is a transaction, not a plain write.
    (hp : u.plain = false)
    -- Its snapshot did not see the earlier slot's write.
    (hsnap : u.snap ≤ s) :
    -- The verdicts: `t` and `mid`, an abort for `u`, then `post`.
    decided store s (t :: mid ++ u :: post) =
      decided store s (t :: mid) ++
        false :: decided (after store s (t :: mid)).1 (after store s (t :: mid)).2 post := by
  -- The later slot does not land against the store it is checked against.
  have hu := no_lost_update store s t u mid k hl hw ht hp hsnap
  -- Its own step: an abort, and the slots after it see the same store.
  have hstep : decided (after store s (t :: mid)).1 (after store s (t :: mid)).2 (u :: post) =
      false :: decided (after store s (t :: mid)).1 (after store s (t :: mid)).2 post := by
    -- Unfold one step and take the aborting branch.
    simp [decided, hu]
  -- Split the ring at the later slot, then use its step.
  rw [decided_append (t :: mid) store s (u :: post), hstep]

/-- **An abort has a cause.** A slot that does not land is a transaction,
and the store it was checked against holds a version of a key it touched that
its snapshot did not see. Nothing aborts for no reason. -/
theorem abort_has_cause (store : List (Nat × Nat)) (u : Txn)
    -- The slot does not land.
    (h : lands store u = false) :
    -- It is a transaction, and a newer version of a touched key exists.
    u.plain = false ∧ ∃ v ∈ store, v.1 ∈ u.touched ∧ u.snap < v.2 := by
  -- Split on whether the slot is a plain write.
  cases hp : u.plain with
  -- Case: a plain write.
  | true =>
    -- A plain write always lands, so this case cannot happen.
    simp [lands, hp] at h
  -- Case: a transaction.
  | false =>
    -- It did not land, so its check found a conflict.
    have hc : conflicts store u = true := by
      -- Unfold `lands` for a transaction.
      simpa [lands, hp] using h
    -- Unfold the conflict into "there is a version such that ...".
    simp only [conflicts, List.any_eq_true, Bool.and_eq_true, List.contains_iff_mem,
      decide_eq_true_eq] at hc
    -- Take that version.
    obtain ⟨v, hv, hk, hlt⟩ := hc
    -- It is the cause.
    exact ⟨rfl, v, hv, hk, hlt⟩

/-! ## The check split at a horizon -/

/-- **Validation ahead of the pipeline.** For a transaction whose snapshot is
at or below `h`, the versions above its snapshot conflict exactly when those
up to `h` (checked on its own thread, before it takes a slot) or those above
`h` (checked when its slot is decided) do. -/
theorem early_check_split (vs : List (Nat × Nat)) (t : Txn) (h : Nat)
    -- The horizon is sampled after the snapshot.
    (hsh : t.snap ≤ h) :
    -- One check equals the two halves.
    conflicts vs t =
      (vs.any (fun v => t.touched.contains v.1 && decide (t.snap < v.2 ∧ v.2 ≤ h)) ||
        vs.any (fun v => t.touched.contains v.1 && decide (h < v.2))) := by
  -- Two booleans are equal when each is true exactly when the other is.
  apply Bool.eq_iff_iff.mpr
  -- Unfold into "there is a version such that ...".
  simp only [conflicts, Bool.or_eq_true, List.any_eq_true, Bool.and_eq_true,
    decide_eq_true_eq]
  -- Prove the two directions.
  constructor
  -- A conflicting version is at or below `h`, or above it.
  · rintro ⟨v, hv, hk, hlt⟩
    -- Look at which.
    by_cases hle : v.2 ≤ h
    -- At or below: the first half finds it.
    · exact Or.inl ⟨v, hv, hk, hlt, hle⟩
    -- Above: the second half finds it.
    · exact Or.inr ⟨v, hv, hk, by omega⟩
  -- Either half's version conflicts in the one check.
  · rintro (⟨v, hv, hk, hlt, -⟩ | ⟨v, hv, hk, hgt⟩)
    -- From the first half: it is above the snapshot.
    · exact ⟨v, hv, hk, hlt⟩
    -- From the second: above `h`, so above the snapshot.
    · exact ⟨v, hv, hk, by omega⟩

/-- **The early check as the code runs it.** The early check does not look
at exactly the versions up to `h`: it looks at the view it loads after
sampling `h`, which holds every version up to `h` that is newer than the
snapshot, and may hold newer ones too (applied, not yet visible). Whichever
thread decides the slot then looks at every version above `h`. Together they
decide as one check of every version. When nothing was made visible since
the snapshot (`h` is the snapshot), the code loads no view at all: `early`
is empty, and the covering hypothesis holds vacuously. -/
theorem early_check_cover (vs early : List (Nat × Nat)) (t : Txn) (h : Nat)
    -- The early view holds only real versions.
    (hsub : ∀ v ∈ early, v ∈ vs)
    -- It holds every version newer than the snapshot up to `h`.
    (hcov : ∀ v ∈ vs, t.snap < v.2 → v.2 ≤ h → v ∈ early)
    -- The horizon is sampled after the snapshot.
    (hsh : t.snap ≤ h) :
    -- One check equals the early check plus the check above `h`.
    conflicts vs t =
      (conflicts early t || vs.any (fun v => t.touched.contains v.1 && decide (h < v.2))) := by
  -- Two booleans are equal when each is true exactly when the other is.
  apply Bool.eq_iff_iff.mpr
  -- Unfold into "there is a version such that ...".
  simp only [conflicts, Bool.or_eq_true, List.any_eq_true, Bool.and_eq_true,
    decide_eq_true_eq]
  -- Prove the two directions.
  constructor
  -- A conflicting version is at or below `h`, or above it.
  · rintro ⟨v, hv, hk, hlt⟩
    -- Look at which.
    by_cases hle : v.2 ≤ h
    -- At or below: the early view holds it, and it conflicts there.
    · exact Or.inl ⟨v, hcov v hv hlt hle, hk, hlt⟩
    -- Above: the check at the slot's decision finds it.
    · exact Or.inr ⟨v, hv, hk, by omega⟩
  -- Either check's version conflicts in the one check.
  · rintro (⟨v, hv, hk, hlt⟩ | ⟨v, hv, hk, hgt⟩)
    -- The early view's version is a real one, above the snapshot.
    · exact ⟨v, hsub v hv, hk, hlt⟩
    -- Above `h`, so above the snapshot.
    · exact ⟨v, hv, hk, by omega⟩

/-! ## Nothing handed out above the horizon: the look above it is skipped -/

/-
The code (`RegolithEngine::validate` and `decide_txn` in
`src/engine/commit`) knows `ceiling`, the last sequence handed out before the
slot it decides (`base.next_seq - 1`). When `ceiling` is at or below the
horizon `h` of the transaction's early check, it does not look above `h` at
all. Story: a baker serves ticket 6 and knows the last ticket served before
it was ticket 5; the customer already checked the board up to ticket 5. There
is nothing newer to look at, so nobody walks to the board again. The three
facts below say why that is exact: the store never holds a version above the
last sequence handed out, so the look above `h` finds nothing, so the check
is the early check alone.
-/

/-- **The ceiling bounds the store.** When no version in the store is newer
than `s`, the last sequence handed out, none is newer than the last sequence
handed out after any number of slots are decided: a slot that lands puts its
writes at the next sequence, which becomes the last one handed out. Every
sequence the code hands out goes through the pipeline's state
(`After::next_seq`: a slot's operations, a counter reservation, an ingest's,
a rollback's floor), so this is the code's `ceiling`. -/
theorem store_below_ceiling (ts : List Txn) :
    -- For every store and last sequence...
    ∀ (store : List (Nat × Nat)) (s : Nat),
      -- ...if no version is newer than the last sequence...
      (∀ v ∈ store, v.2 ≤ s) →
        -- ...then after the slots none is newer than the new last sequence.
        ∀ v ∈ (after store s ts).1, v.2 ≤ (after store s ts).2 := by
  -- By induction on the slots, for every store and sequence.
  induction ts with
  -- Case: no slot.
  | nil =>
    -- Take the store, the sequence, the bound, a version and where it is.
    intro store s hb v hv
    -- Nothing was decided: the store and the sequence are the same.
    simp only [after] at hv ⊢
    -- So the bound we were given is the one asked for.
    exact hb v hv
  -- Case: a first slot and then the rest, the claim known for the rest (ih).
  | cons t rest ih =>
    -- Take the store, the sequence and the bound.
    intro store s hb
    -- Does the first slot land?
    by_cases hl : lands store t = true
    -- It lands: its writes join the store at `s + 1`.
    · rw [after_cons_lands store s t rest hl]
      -- The rest follow by induction, once the bigger store is below `s + 1`.
      apply ih
      -- Take a version of the bigger store and where it is.
      intro v hv
      -- It is one of the new writes, or one of the old versions.
      rcases List.mem_append.mp hv with hw | hs
      -- A new write: it is at `s + 1` itself.
      · obtain ⟨k, _, rfl⟩ := List.mem_map.mp hw
        -- `s + 1` is at or below `s + 1`.
        simp
      -- An old version: at or below `s`, so below `s + 1`.
      · exact Nat.le_succ_of_le (hb v hs)
    -- It aborts: the store and the sequence are unchanged.
    · simp only [Bool.not_eq_true] at hl
      -- Take the aborting step.
      rw [after_cons_aborts store s t rest hl]
      -- The rest follow by induction from the same bound.
      exact ih store s hb

/-- **Nothing above the ceiling.** When every version is at or below `c` and
`c` is at or below the horizon `h`, the look above `h` finds nothing. -/
theorem nothing_above_ceiling (vs : List (Nat × Nat)) (t : Txn) (h c : Nat)
    -- No version is newer than the ceiling.
    (hb : ∀ v ∈ vs, v.2 ≤ c)
    -- The ceiling is at or below the horizon.
    (hch : c ≤ h) :
    -- The look above the horizon finds no version.
    vs.any (fun v => t.touched.contains v.1 && decide (h < v.2)) = false := by
  -- Unfold into "no version is a touched key above the horizon".
  simp only [List.any_eq_false, Bool.and_eq_true, List.contains_iff_mem, decide_eq_true_eq,
    not_and]
  -- Take a version, where it is, and that its key was touched.
  intro v hv _
  -- It is at or below the ceiling...
  have hle := hb v hv
  -- ...so at or below the horizon, not above it.
  omega

/-- **The skip is exact.** With the early check's view as in
`early_check_cover`, and every version at or below a ceiling that is at or
below the horizon, the one check of every version is the early check alone:
the slot's decision needs no lookup. -/
theorem ceiling_skips_the_check_above (vs early : List (Nat × Nat)) (t : Txn) (h c : Nat)
    -- The early view holds only real versions.
    (hsub : ∀ v ∈ early, v ∈ vs)
    -- It holds every version newer than the snapshot up to `h`.
    (hcov : ∀ v ∈ vs, t.snap < v.2 → v.2 ≤ h → v ∈ early)
    -- The horizon is sampled after the snapshot.
    (hsh : t.snap ≤ h)
    -- No version is newer than the ceiling.
    (hb : ∀ v ∈ vs, v.2 ≤ c)
    -- The ceiling is at or below the horizon.
    (hch : c ≤ h) :
    -- One check of every version equals the early check.
    conflicts vs t = conflicts early t := by
  -- Split the one check at the horizon, drop the empty half above it, and
  -- an `or` with `false` is what it was.
  rw [early_check_cover vs early t h hsub hcov hsh, nothing_above_ceiling vs t h c hb hch,
    Bool.or_false]

/-- The check a slot runs when its decision skips the look above the horizon
whenever `guard` is at or below it: the early check alone then, the early
check and the look above the horizon otherwise. The code's guard is the
ceiling; mutant SkipOnPublished's is the published horizon. -/
def checkSkipping (vs early : List (Nat × Nat)) (t : Txn) (h guard : Nat) : Bool :=
  -- The guard allows the skip: the early check's verdict alone.
  if guard ≤ h then conflicts early t
  -- Otherwise both halves.
  else conflicts early t || vs.any (fun v => t.touched.contains v.1 && decide (h < v.2))

/-- **The skip with the ceiling as its guard is the one check.** -/
theorem skip_on_ceiling_is_one_check (vs early : List (Nat × Nat)) (t : Txn) (h c : Nat)
    -- The early view holds only real versions.
    (hsub : ∀ v ∈ early, v ∈ vs)
    -- It holds every version newer than the snapshot up to `h`.
    (hcov : ∀ v ∈ vs, t.snap < v.2 → v.2 ≤ h → v ∈ early)
    -- The horizon is sampled after the snapshot.
    (hsh : t.snap ≤ h)
    -- No version is newer than the ceiling.
    (hb : ∀ v ∈ vs, v.2 ≤ c) :
    -- Skipping on the ceiling decides as one check of every version.
    checkSkipping vs early t h c = conflicts vs t := by
  -- Look at whether the guard allows the skip.
  by_cases hch : c ≤ h
  -- It does: take the skipping branch of the `if`.
  · simp only [checkSkipping, hch, ↓reduceIte]
    -- The early check alone, which is the one check.
    exact (ceiling_skips_the_check_above vs early t h c hsub hcov hsh hb hch).symm
  -- It does not: take the other branch of the `if`.
  · simp only [checkSkipping, hch, ↓reduceIte]
    -- Both halves, which is the one check by `early_check_cover`.
    exact (early_check_cover vs early t h hsub hcov hsh).symm

/-- **RED SkipOnPublished.** The skip guarded by the published horizon. Slot
1 committed key 1 at sequence 1 and is not published yet, so the published
horizon is 0, and so is the horizon of the early check of a transaction that
read key 1 at snapshot 0: its early view holds nothing. The broken guard
(published 0 at or below horizon 0) skips, finds no conflict, and the
transaction commits over slot 1's write: a lost update. The ceiling (1) is
above the horizon, so the design looks and aborts it, as the one check does.
-/
theorem skip_on_published_loses_an_update :
    -- The broken guard lets it commit,
    checkSkipping [(1, 1)] [] ⟨0, [1], [1], false⟩ 0 0 = false ∧
      -- the design's guard aborts it,
      checkSkipping [(1, 1)] [] ⟨0, [1], [1], false⟩ 0 1 = true ∧
        -- which is what one check of every version says.
        conflicts [(1, 1)] ⟨0, [1], [1], false⟩ = true := by
  -- Concrete lists and numbers: evaluate.
  decide

/-! ## The RED case: checking against what is published only -/

/-- Mutant ValidatePublished: each slot checked against the published
versions alone, a view that lacks the writes of the slots decided before it
and not yet published. -/
def publishedOnly (view : List (Nat × Nat)) : List Txn → List Bool
  -- No slot left: no verdict.
  | [] => []
  -- The verdict ignores every other slot.
  | t :: rest => lands view t :: publishedOnly view rest

/-- Two transactions that both began at sequence 0 and write key 1. -/
def twoWriters : List Txn :=
  -- Snapshot 0, touching key 1, writing key 1, not plain; twice.
  [⟨0, [1], [1], false⟩, ⟨0, [1], [1], false⟩]

/-- **`published_only_loses_an_update`.** With nothing published yet, the
check against the published versions alone commits both writers of key 1, a
lost update; deciding them in ring order against the store aborts the
second. -/
theorem published_only_loses_an_update :
    -- The bug commits both,
    publishedOnly [] twoWriters = [true, true] ∧
      -- and the check in ring order aborts the second.
      decided [] 0 twoWriters = [true, false] := by
  -- Both sides are computations on concrete lists: evaluate them.
  decide

/-- A plain write of key 1 ahead of a transaction that wrote key 1 from
snapshot 0: the plain write lands unchecked and is in the store, so the
transaction behind it aborts. -/
theorem plain_write_is_seen :
    -- The plain write commits and the transaction behind it aborts.
    decided [] 0 [⟨0, [], [1], true⟩, ⟨0, [1], [1], false⟩] = [true, false] := by
  -- Concrete lists: evaluate.
  decide

/-- Installing a table must raise the same view's ceiling to cover every
new version. Flush and ingest establish this before publishing the view;
compaction carries forward versions already covered by the old ceiling. -/
theorem installed_tables_below_ceiling
    -- The existing and incoming tables each have a sequence ceiling.
    (old added : List (Nat × Nat)) (ceiling newest : Nat)
    -- The published view already bounds every existing table version.
    (oldBound : ∀ v ∈ old, v.2 ≤ ceiling)
    -- The writer or ingestion stamp bounds every added version.
    (newBound : ∀ v ∈ added, v.2 ≤ newest) :
    -- Raising the published ceiling covers both sets of tables.
    ∀ v ∈ old ++ added, v.2 ≤ max ceiling newest := by
  -- Pick any version in the resulting view.
  intro v member
  -- It came from an existing table or an added table.
  rcases List.mem_append.mp member with before | after
  -- Existing versions remain below the raised ceiling.
  · exact Nat.le_trans (oldBound v before) (Nat.le_max_left _ _)
  -- Added versions are below the ceiling supplied by their writer.
  · exact Nat.le_trans (newBound v after) (Nat.le_max_right _ _)

/-- A table ceiling at or below the transaction's observation excludes every
conflicting table version. `latest_version_after` may visit only the memtables
when `ReadView.version.last_seq` is at or below that observation. The ceiling
belongs to the same loaded view as the sources; a concurrent flush publishes
another view and cannot move a newer version into this one's tables. -/
theorem old_tables_do_not_change_validation
    -- The sources of the loaded view and the transaction being checked.
    (mem tables : List (Nat × Nat)) (t : Txn) (ceiling : Nat)
    -- Every table version is bounded by this view's recorded ceiling.
    (bounded : ∀ v ∈ tables, v.2 ≤ ceiling)
    -- The transaction observed every sequence up to that ceiling.
    (observed : ceiling ≤ t.snap) :
    -- Looking at old tables cannot change conflict detection.
    conflicts (mem ++ tables) t = conflicts mem t := by
  -- The complete check is the disjunction of the checks of its sources.
  simp only [conflicts, List.any_append]
  -- No table version is above the observation, so that disjunct is false.
  rw [nothing_above_ceiling tables t t.snap ceiling bounded observed, Bool.or_false]

-- The end of this file's names.
end Regolith.PipelineCheck
