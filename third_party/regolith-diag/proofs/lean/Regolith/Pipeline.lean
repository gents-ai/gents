/-!
# Pipeline: the lock-free commit pipeline, for every number of slots

This file backs the TLA+ model `proofs/tla/CommitPipeline.tla` and the code
in `src/engine/commit`.

The pipeline keeps commits in a ring of slots. Think of a bakery with a
ticket machine: every commit pulls a numbered ticket, orders are handled
strictly in ticket order, and whoever stands at the counter does the next
step of the next order, its own or somebody else's.

TLC checks the interleavings of two or three threads. The theorems here
hold for every number of slots and every pattern of verdicts and stages.

What is proved, in plain words:

1. `seqs_dense`, `seqs_strictly_increasing` (TLA+ `NoHole`,
   `SeqInRingOrder`): deciding in ring order hands the slots that land
   something the sequences `base+1, base+2, ...` with no gap, in ring
   order. An abort, a void slot and a slot a rollback threw out take
   nothing, so none of them leaves a hole.
2. `publication_order`, `reader_sees_published` (`PublishedPrefix`,
   `ReadersSeePrefix`): publishing the longest publishable prefix
   publishes exactly `base+1, ..., horizon`, and a reader at the horizon
   sees exactly those, given that a slot is applied before the decided
   frontier passes it (P2).
3. `rollback_as_if_never_decided`, `seqs_after_rollback` (`NoHole` across
   a rollback): once a rollback has thrown slots out, every slot kept and
   every slot decided later holds the sequence it would hold had the
   discarded slots never been decided.
4. `rollback_keeps_published`, `rollback_finishes_everything`,
   `rollback_keeps_horizon` (G2): a rollback changes nothing in the
   published prefix, leaves every slot it dealt with finished, and shows
   readers nothing more and nothing less.
5. `sync_covers_only_written` (`DurableImpliesWritten`): a sync that
   vouches for the written prefix of the log vouches for no commit whose
   record is not written.
6. The RED configurations as concrete counterexamples:
   `abort_with_seq_leaves_hole`, `fetch_max_shows_unpublished`,
   `publish_before_apply_hides_commit`, `sync_past_gap_covers_unwritten`.

Who may take which step, and that a stopped thread holds nobody up, is
`Regolith/PipelineHelping.lean`. What stays in TLA+: that the
interleavings of several threads keep finishing slots (the liveness half),
the compare-and-swaps, the freeze of the frontiers during a rollback and
the epochs that mark a stale verdict. The check a slot runs when it is
decided is `Regolith/PipelineCheck.lean`.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. Inside a proof, each step is commented with what it establishes.
-/

-- Everything below is named Regolith.Pipeline.<name>.
namespace Regolith.Pipeline

/-! ## The objects -/

/-
Sequence numbers are plain natural numbers (`Nat`), kept as `Nat` rather
than a named alias so that `omega` sees them as numbers. `s` is the last
sequence handed out before the slots under discussion: the base.
-/

/-- What a decided slot became. -/
inductive Dec where
  /-- A write or a transaction that passed validation: it takes the next
  sequence and a record in the log. -/
  | commit
  /-- An ordered unit that installs a table (an ingest): it takes the next
  sequence and no record. -/
  | table
  /-- A slot that lands nothing: it lost a conflict, was refused, or is an
  ordered unit that brings no keys (a rotation, a column family's
  creation). It takes nothing. -/
  | abort
  /-- A helper voided the slot, reserved but never filled: it takes nothing. -/
  | void
  /-- A rollback threw the slot out: whatever it took was handed back. -/
  | failed
  -- Two such values can be compared for equality.
  deriving DecidableEq

/-- Whether a verdict takes a sequence. -/
def takes : Dec → Bool
  -- A commit takes the next sequence.
  | .commit => true
  -- So does an installed table.
  | .table => true
  -- An abort takes none.
  | .abort => false
  -- A void slot takes none.
  | .void => false
  -- A slot a rollback threw out holds none any more.
  | .failed => false

/-- A decided slot of the ring, in ring order. -/
structure Slot where
  /-- The verdict. -/
  dec : Dec
  /-- Its versions are in the memtable. -/
  applied : Bool
  /-- Its record is in the log (meaningful for a commit). -/
  written : Bool
  /-- Its durability allows publication: it is durable, or it did not ask
  for Immediate (meaningful for a commit). -/
  durableOk : Bool
  -- Two such values can be compared for equality.
  deriving DecidableEq

/-! ## Deciding in ring order -/

/-- `seqs s slots`: the sequence each slot takes when the slots are decided
one after another in ring order, after base `s`. A slot that takes one
takes the next; the others take none (`none`). -/
def seqs : Nat → List Slot → List (Option Nat)
  -- No slot, no sequence.
  | _, [] => []
  -- Look at the first slot.
  | s, sl :: rest =>
    -- A taker takes `s + 1` and the slots after it continue from there.
    if takes sl.dec then some (s + 1) :: seqs (s + 1) rest
    -- Any other slot takes nothing and the slots after it continue from `s`.
    else none :: seqs s rest

/-- How many slots in the list take a sequence. -/
def taken : List Slot → Nat
  -- No slot, none taken.
  | [] => 0
  -- A taker counts one; any other slot counts nothing.
  | sl :: rest => if takes sl.dec then taken rest + 1 else taken rest

/-- **No hole.** The sequences the slots take, read in ring order with the
slots that take none skipped, are exactly `s+1, s+2, ..., s+n` for the `n`
takers: consecutive, from the base, with no gap where an abort, a void
slot or a discarded slot sits. -/
theorem seqs_dense (s : Nat) (l : List Slot) :
    -- The claim itself; its proof follows.
    (seqs s l).filterMap id = List.range' (s + 1) (taken l) := by
  -- By induction on the slots, for every base `s`.
  induction l generalizing s with
  -- Case: the empty list.
  | nil =>
    -- No slot: no sequence, and an empty range.
    simp [seqs, taken]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- Does the first slot take a sequence?
    by_cases h : takes sl.dec = true
    -- It takes `s + 1`; the rest take `s+2, ...` by induction.
    ·
      -- `range' (s+1) (n+1)` is `s+1` followed by `range' (s+2) n`.
      simp [seqs, taken, h, ih (s + 1), List.range'_succ]
    -- It takes nothing: the rest give the whole answer.
    · simp [seqs, taken, h, ih s]

/-- A range of consecutive naturals is strictly increasing. -/
theorem range'_strictly_increasing (a n : Nat) : (List.range' a n).Pairwise (· < ·) := by
  -- By induction on the length, for every start `a`.
  induction n generalizing a with
  -- Case: zero.
  | zero =>
    -- The empty range is trivially increasing.
    simp
  -- Case: one more, the claim known for the number before (ih).
  | succ n ih =>
    -- `a` comes first, and every later element is at least `a + 1`.
    rw [List.range'_succ, List.pairwise_cons]
    -- The tail is increasing by induction; the head is below all of it.
    refine ⟨?_, ih (a + 1)⟩
    -- Take any later element.
    intro x hx
    -- An element of `range' (a+1) n` lies in `[a+1, a+1+n)`.
    have := (List.mem_range'_1.mp hx).1
    -- So it is above `a`.
    omega

/-- **Sequence order is ring order.** Read in ring order, the sequences the
slots take strictly increase. -/
theorem seqs_strictly_increasing (s : Nat) (l : List Slot) :
    -- The claim itself; its proof follows.
    ((seqs s l).filterMap id).Pairwise (· < ·) := by
  -- They are a range of consecutive naturals.
  rw [seqs_dense]
  -- And such a range is strictly increasing.
  exact range'_strictly_increasing _ _

/-- Deciding a prefix of the ring gives the prefix of the decisions: a
slot's sequence depends only on the slots before it. -/
theorem seqs_take (s n : Nat) (l : List Slot) :
    -- The claim itself; its proof follows.
    seqs s (l.take n) = (seqs s l).take n := by
  -- By induction on the slots, for every base and every prefix length.
  induction l generalizing s n with
  -- Case: the empty list.
  | nil =>
    -- No slot: both sides are empty.
    simp [seqs]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- A prefix of length zero is empty on both sides.
    cases n with
    -- Case: zero.
    | zero => simp [seqs]
    -- Case: one more, the claim known for the number before (ih).
    | succ n =>
      -- Otherwise the first slot is decided the same way on both sides,
      -- taker or not, and the rest follow by induction.
      by_cases h : takes sl.dec = true <;> simp [seqs, h, ih]

/-- Takers are counted slot by slot: a list followed by another holds the
takers of both. -/
theorem taken_append (a b : List Slot) : taken (a ++ b) = taken a + taken b := by
  -- By induction on the first list.
  induction a with
  -- Case: the empty list.
  | nil =>
    -- Nothing in front: the count is the second list's.
    simp [taken]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- The first slot counts the same on both sides, taker or not.
    by_cases h : takes sl.dec = true <;> simp [taken, h, ih] <;> omega

/-- The slots after a list are numbered from where the list left off: the
base plus the list's takers. -/
theorem seqs_append (s : Nat) (a b : List Slot) :
    -- The claim itself; its proof follows.
    seqs s (a ++ b) = seqs s a ++ seqs (s + taken a) b := by
  -- By induction on the first list, for every base.
  induction a generalizing s with
  -- Case: the empty list.
  | nil =>
    -- Nothing in front: the second list is numbered from `s`.
    simp [seqs, taken]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- Does the first slot take a sequence?
    by_cases h : takes sl.dec = true
    -- It takes `s + 1`, so the rest start one later: the same total.
    · have hb : s + 1 + taken rest = s + (taken rest + 1) := by omega
      -- Unfold both sides and use the induction step from base `s + 1`.
      simp [seqs, taken, h, ih (s + 1), hb]
    -- It takes nothing: the rest start from `s` on both sides.
    · simp [seqs, taken, h, ih s]

/-! ## Publishing the longest publishable prefix -/

/-- Whether a slot may be published. A commit waits for its record to be
written and, when it asked, durable; every other slot has nothing to wait
for. Its versions are in the memtable already: that happened before the
next slot was decided (P2). -/
def publishable (sl : Slot) : Bool :=
  -- One case for each shape it can have.
  match sl.dec with
  -- A commit: written, and durable enough.
  | .commit => sl.written && sl.durableOk
  -- A table is installed when its unit runs.
  | .table => true
  -- An abort is published at once.
  | .abort => true
  -- So is a void slot.
  | .void => true
  -- So is a discarded slot: its owner only has to be told.
  | .failed => true

/-- `frontier slots`: how many leading slots may be published. It stops at
the first slot that is not publishable. -/
def frontier : List Slot → Nat
  -- No slot, nothing to publish.
  | [] => 0
  -- Past a publishable slot and on; stopped by any other.
  | sl :: rest => if publishable sl then frontier rest + 1 else 0

/-- The visible horizon after publishing the frontier: the base plus the
number of takers the frontier passed. -/
def horizon (s : Nat) (l : List Slot) : Nat := s + taken (l.take (frontier l))

/-- **Publication order is sequence order.** Publishing the longest
publishable prefix publishes, in ring order, exactly the sequences
`s+1, ..., horizon`: each publication is the next sequence. -/
theorem publication_order (s : Nat) (l : List Slot) :
    -- The claim itself; its proof follows.
    (seqs s (l.take (frontier l))).filterMap id = List.range' (s + 1) (horizon s l - s) := by
  -- The published prefix is a list of slots like any other, so its
  -- sequences are dense.
  rw [seqs_dense]
  -- And `horizon s l - s` is its count of takers.
  simp [horizon]

/-! ## What a reader at the horizon sees -/

/-- `seen s v slots`: what a reader at horizon `v` sees of the slots, in
ring order: the sequence of every taker whose versions are applied and
whose sequence is at most `v`. Visibility asks only that the versions be in
the memtable and old enough; it does not ask whether the slot was
published. -/
def seen (s v : Nat) : List Slot → List Nat
  -- No slot, nothing seen.
  | [] => []
  -- Look at the first slot.
  | sl :: rest =>
    -- A taker holds sequence `s + 1`...
    if takes sl.dec then
      -- ...which is seen when applied and at or below `v`.
      (if sl.applied && decide (s + 1 ≤ v) then [s + 1] else []) ++ seen (s + 1) v rest
    -- A slot that takes nothing holds no version.
    else seen s v rest

/-- A reader at a horizon `v` no later than the base sees nothing: every
sequence after the base is above it. -/
theorem seen_at_or_below_base {t v : Nat} (l : List Slot)
    -- The horizon is at or below the base.
    (hv : v ≤ t) : seen t v l = [] := by
  -- By induction on the slots, for every base at or above `v`.
  induction l generalizing t with
  -- Case: the empty list.
  | nil =>
    -- No slot, nothing seen.
    simp [seen]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- Does the first slot take a sequence?
    by_cases h : takes sl.dec = true
    -- `t + 1` is above `v`...
    · have h1 : ¬ (t + 1 ≤ v) := by omega
      -- ...and so is every later sequence, by induction from base `t + 1`.
      simp [seen, h, h1, ih (Nat.le_succ_of_le hv)]
    -- It holds no version; the rest show nothing by induction.
    · simp [seen, h, ih hv]

/-- P2 as a property of the slots the decided frontier has passed: every
one that takes a sequence is applied. -/
def AllApplied (l : List Slot) : Prop := ∀ sl ∈ l, takes sl.dec = true → sl.applied = true

/-- The core of `reader_sees_published`, by induction: a reader at the
horizon sees the consecutive sequences of the published takers. -/
theorem seen_horizon (s : Nat) (l : List Slot)
    -- Every slot was applied before the decided frontier passed it.
    (hP2 : AllApplied l) :
    -- The claim itself; its proof follows.
    seen s (horizon s l) l = List.range' (s + 1) (taken (l.take (frontier l))) := by
  -- By induction on the slots, for every base `s`.
  induction l generalizing s with
  -- Case: the empty list.
  | nil =>
    -- No slot: nothing seen, an empty range.
    simp [seen, frontier, taken]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- P2 holds for the rest of the slots too.
    have hrest : AllApplied rest := fun x hx => hP2 x (List.mem_cons_of_mem _ hx)
    -- Is the first slot published?
    by_cases hp : publishable sl = true
    -- It is. Does it take a sequence?
    · by_cases ht : takes sl.dec = true
      -- By P2 it is applied.
      · have ha : sl.applied = true := hP2 sl (List.mem_cons_self) ht
        -- It takes `s + 1`, and the horizon is the rest's horizon from
        -- base `s + 1`.
        have hh : horizon s (sl :: rest) = horizon (s + 1) rest := by
          -- Unfold both horizons; the counts differ by the one taker.
          simp [horizon, frontier, taken, hp, ht]
          -- Plain arithmetic.
          omega
        -- Read at that horizon.
        rw [hh]
        -- `s + 1` is at or below it.
        have hle : s + 1 ≤ horizon (s + 1) rest := by
          -- A horizon is its base plus a count of takers.
          simp [horizon]
        -- The reader sees `s + 1`, then what the rest shows by induction.
        simp [seen, ht, ha, hle, ih (s + 1) hrest, frontier, taken, hp, List.range'_succ]
      -- It takes nothing: the horizon is the rest's, from the same base.
      · have hh : horizon s (sl :: rest) = horizon s rest := by
          -- Unfold both horizons: the first slot adds no taker.
          simp [horizon, frontier, taken, hp, ht]
        -- Read at that horizon.
        rw [hh]
        -- It shows nothing itself; the rest shows the answer by induction.
        simp [seen, ht, ih s hrest, frontier, taken, hp]
    -- It is not published, so the frontier is 0...
    · have hf : frontier (sl :: rest) = 0 := by
        -- The first slot stops the frontier.
        simp [frontier, hp]
      -- ...and the horizon is the base.
      have hh : horizon s (sl :: rest) = s := by
        -- An empty prefix holds no taker.
        simp [horizon, hf, taken]
      -- Read at the base, with nothing published.
      rw [hh, hf]
      -- Does the unpublished slot take a sequence?
      by_cases ht : takes sl.dec = true
      -- Its sequence `s + 1` is above the base...
      · have h1 : ¬ (s + 1 ≤ s) := by omega
        -- ...so the reader sees neither it nor anything after it.
        simp [seen, ht, h1, seen_at_or_below_base rest (Nat.le_succ s), taken]
      -- It holds no version, and nothing after it is at or below the base.
      · simp [seen, ht, seen_at_or_below_base rest (Nat.le_refl s), taken]

/-- **A reader at the published horizon sees exactly the takers below the
frontier.** What it sees is the list of sequences the published prefix
took, `s+1, ..., horizon` in ring order: every published commit and table,
and nothing from a later slot, even one already applied. -/
theorem reader_sees_published (s : Nat) (l : List Slot)
    -- Every slot was applied before the decided frontier passed it.
    (hP2 : AllApplied l) :
    -- The claim itself; its proof follows.
    seen s (horizon s l) l = (seqs s (l.take (frontier l))).filterMap id ∧
      seen s (horizon s l) l = List.range' (s + 1) (horizon s l - s) := by
  -- Both sides are the same consecutive range.
  refine ⟨?_, ?_⟩
  -- What is seen is that range, and so are the prefix's sequences.
  · rw [seen_horizon s l hP2, seqs_dense]
  -- What is seen is that range...
  · rw [seen_horizon s l hP2]
    -- ...whose length is the horizon minus the base.
    simp [horizon]

/-! ## A rollback discards a suffix -/

/-- Throw a slot out: whatever its verdict was, it now holds nothing. -/
def fail (sl : Slot) : Slot := { sl with dec := .failed }

/-- What a rollback leaves of the slots decided and not yet published: the
longest publishable prefix as it is, and every slot after it thrown out. -/
def rollback (l : List Slot) : List Slot :=
  -- The prefix as it is, then every slot after it failed.
  l.take (frontier l) ++ (l.drop (frontier l)).map fail

/-- Slots that were thrown out take nothing. -/
theorem taken_map_fail (l : List Slot) : taken (l.map fail) = 0 := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- No slot, none taken.
    simp [taken]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- A discarded slot is no taker, and the rest take none by induction.
    simp [taken, fail, takes, ih]

/-- Slots that were thrown out hold no sequence, and leave the base where
it was. -/
theorem seqs_map_fail (s : Nat) (l : List Slot) :
    -- The claim itself; its proof follows.
    seqs s (l.map fail) = l.map (fun _ => none) := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- No slot, no sequence.
    simp [seqs]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- A discarded slot holds `none`, and the rest follow by induction.
    simp [seqs, fail, takes, ih]

/-- **After a rollback, the numbering is that of the kept slots alone.**
With `kept` published, `dropped` thrown out and `more` decided afterwards:
the kept slots keep their sequences, the discarded ones hold none, and the
later ones are numbered from the base plus the kept takers. The sequences
the discarded slots held are handed out again. -/
theorem seqs_after_rollback (s : Nat) (kept dropped more : List Slot) :
    -- The claim itself; its proof follows.
    seqs s (kept ++ dropped.map fail ++ more) =
      seqs s kept ++ dropped.map (fun _ => none) ++ seqs (s + taken kept) more := by
  -- Number the three parts one after another.
  rw [seqs_append, seqs_append]
  -- The discarded part holds no sequence and takes none.
  simp [seqs_map_fail, taken_append, taken_map_fail]

/-- **A rollback leaves the sequences as if the discarded slots had never
been decided** (`NoHole` across a rollback). The sequences in use are the
ones the kept slots and the later slots would hold with the discarded
slots absent: consecutive from the base, with no gap and none used twice. -/
theorem rollback_as_if_never_decided (s : Nat) (kept dropped more : List Slot) :
    -- The claim itself; its proof follows.
    (seqs s (kept ++ dropped.map fail ++ more)).filterMap id =
      (seqs s (kept ++ more)).filterMap id := by
  -- Both sides are consecutive ranges from the base...
  rw [seqs_dense, seqs_dense]
  -- ...of the same length: the discarded slots take nothing.
  simp [taken_append, taken_map_fail]

/-- A publishable prefix is published whole: the frontier of the slots the
frontier passed is all of them. -/
theorem frontier_take (l : List Slot) :
    -- The claim itself; its proof follows.
    frontier (l.take (frontier l)) = (l.take (frontier l)).length := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- No slot: zero on both sides.
    simp [frontier]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- Is the first slot publishable?
    by_cases hp : publishable sl = true
    -- It is: it is in the prefix, and the rest follows by induction.
    · simp [frontier, hp, ih]
    -- It is not: the prefix is empty.
    · simp [frontier, hp]

/-- The frontier of a list that begins with a wholly publishable part runs
through that part and on into what follows. -/
theorem frontier_append (a b : List Slot)
    -- Every slot of the first part is publishable.
    (ha : frontier a = a.length) : frontier (a ++ b) = a.length + frontier b := by
  -- By induction on the first part.
  induction a with
  -- Case: the empty list.
  | nil =>
    -- Nothing in front: the frontier is the second part's.
    simp
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- Is the first slot publishable?
    by_cases hp : publishable sl = true
    -- It is, so the rest of the first part is wholly publishable too.
    · have hrest : frontier rest = rest.length := by
        -- Read it off the hypothesis for the whole part.
        simpa [frontier, hp] using ha
      -- Pass the first slot, then use the induction step.
      simp [frontier, hp, ih hrest]
      -- Plain arithmetic on the lengths.
      omega
    -- It is not: the hypothesis would say `0 = length + 1`.
    · simp [frontier, hp] at ha

/-- Discarded slots are all publishable: each owner only has to be told. -/
theorem frontier_map_fail (l : List Slot) : frontier (l.map fail) = l.length := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- No slot: zero on both sides.
    simp [frontier]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- A discarded slot is publishable, and the rest follow by induction.
    simp [frontier, fail, publishable, ih]

/-- **A rollback touches nothing published** (G2). The slots the frontier
could pass before the rollback are the same slots after it. -/
theorem rollback_keeps_published (l : List Slot) :
    -- The claim itself; its proof follows.
    (rollback l).take (frontier l) = l.take (frontier l) := by
  -- The frontier is no longer than the list.
  have hlen : (l.take (frontier l)).length = frontier l := by
    -- The frontier of a list never exceeds its length.
    have hle : frontier l ≤ l.length := by
      -- By induction on the slots.
      induction l with
      -- Case: the empty list.
      | nil => simp [frontier]
      -- Case: a first element and then the rest, the claim known for the rest (ih).
      | cons sl rest ih =>
        -- A publishable first slot adds one to both; any other gives zero.
        by_cases hp : publishable sl = true <;> simp [frontier, hp] <;> omega
    -- So taking that many slots gives exactly that many.
    simp [List.length_take, Nat.min_eq_left hle]
  -- Taking the kept part's own length from `kept ++ rest` gives `kept`.
  simp [rollback, hlen]

/-- **A rollback leaves nothing waiting.** Every slot it dealt with is
publishable afterwards: the published prefix as before, and the discarded
slots because they hold nothing. So both frontiers can jump past them all
and every owner learns its fate. -/
theorem rollback_finishes_everything (l : List Slot) :
    -- The claim itself; its proof follows.
    frontier (rollback l) = l.length := by
  -- The kept part is wholly publishable, the discarded part too.
  rw [rollback, frontier_append _ _ (frontier_take l), frontier_map_fail]
  -- The two parts together are the whole list.
  simp [List.length_take, List.length_drop]
  -- Plain arithmetic: a prefix and what is left after it.
  omega

/-- **A rollback shows readers nothing more and nothing less.** The
horizon after it is the horizon before it: the discarded slots take
nothing, so passing them raises nothing. -/
theorem rollback_keeps_horizon (s : Nat) (l : List Slot) :
    -- The claim itself; its proof follows.
    horizon s (rollback l) = horizon s l := by
  -- After the rollback the frontier passes every slot.
  have hf : frontier (rollback l) = l.length := rollback_finishes_everything l
  -- And the rollback leaves as many slots as there were.
  have hlen : (rollback l).length = l.length := by
    -- A prefix, and the rest of the list mapped slot by slot.
    simp [rollback, List.length_take, List.length_drop]
    -- Plain arithmetic.
    omega
  -- So the horizon counts the takers of the whole result...
  have hall : (rollback l).take (frontier (rollback l)) = rollback l := by
    -- The frontier is the list's own length...
    rw [hf, ← hlen]
    -- ...and taking a list's own length gives the list.
    exact List.take_length
  -- Spell both horizons out.
  show s + taken ((rollback l).take (frontier (rollback l))) = s + taken (l.take (frontier l))
  -- The left one counts the takers of everything the rollback left...
  rw [hall]
  -- ...which are the kept part's takers and the discarded part's none.
  rw [rollback, taken_append, taken_map_fail]
  -- Adding zero changes nothing.
  omega

/-! ## The sync vouches for the written prefix -/

/-- Whether a slot has nothing left to do with the log: a commit once its
record is written, every other slot at once (it writes no record). -/
def walDone (sl : Slot) : Bool :=
  -- One case for each shape it can have.
  match sl.dec with
  -- A commit is done once its record is in the log.
  | .commit => sl.written
  -- A table is not a log record.
  | .table => true
  -- An abort writes nothing.
  | .abort => true
  -- A void slot writes nothing.
  | .void => true
  -- A discarded slot's record was cut from the log.
  | .failed => true

/-- How far the log is written with no gap: how many leading slots are
done with the log. -/
def writtenTo : List Slot → Nat
  -- No slot, nothing written.
  | [] => 0
  -- Past a slot that is done and on; stopped by a record not written.
  | sl :: rest => if walDone sl then writtenTo rest + 1 else 0

/-- What a sync vouches for: the last sequence in the written prefix. -/
def syncTarget (s : Nat) (l : List Slot) : Nat := s + taken (l.take (writtenTo l))

/-- The sequences, at or below `v`, of the commits whose record is not in
the log: what a durable mark at `v` would wrongly cover. -/
def unwrittenUpTo (s v : Nat) : List Slot → List Nat
  -- No slot, nothing uncovered.
  | [] => []
  -- Look at the first slot.
  | sl :: rest =>
    -- A taker holds sequence `s + 1`...
    if takes sl.dec then
      -- ...which counts when the slot is not done with the log and the
      -- sequence is at or below `v`.
      (if !walDone sl && decide (s + 1 ≤ v) then [s + 1] else []) ++ unwrittenUpTo (s + 1) v rest
    -- A slot that takes nothing has no sequence to cover.
    else unwrittenUpTo s v rest

/-- A mark at or below the base covers nothing after the base. -/
theorem unwritten_at_or_below_base {t v : Nat} (l : List Slot)
    -- The mark is at or below the base.
    (hv : v ≤ t) : unwrittenUpTo t v l = [] := by
  -- By induction on the slots, for every base at or above `v`.
  induction l generalizing t with
  -- Case: the empty list.
  | nil =>
    -- No slot, nothing covered.
    simp [unwrittenUpTo]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- Does the first slot take a sequence?
    by_cases h : takes sl.dec = true
    -- `t + 1` is above the mark...
    · have h1 : ¬ (t + 1 ≤ v) := by omega
      -- ...and so is every later sequence, by induction from `t + 1`.
      simp [unwrittenUpTo, h, h1, ih (Nat.le_succ_of_le hv)]
    -- It takes none; the rest are above the mark by induction.
    · simp [unwrittenUpTo, h, ih hv]

/-- **What a sync vouches for is written** (`DurableImpliesWritten`). A
durable mark at the end of the written prefix covers no commit whose
record is missing from the log: every sequence at or below it belongs to a
slot inside that prefix. -/
theorem sync_covers_only_written (s : Nat) (l : List Slot) :
    -- The claim itself; its proof follows.
    unwrittenUpTo s (syncTarget s l) l = [] := by
  -- By induction on the slots, for every base `s`.
  induction l generalizing s with
  -- Case: the empty list.
  | nil =>
    -- No slot, nothing covered.
    simp [unwrittenUpTo]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons sl rest ih =>
    -- Is the first slot done with the log?
    by_cases hw : walDone sl = true
    -- It is. Does it take a sequence?
    · by_cases ht : takes sl.dec = true
      -- The mark is the rest's mark from base `s + 1`.
      · have hh : syncTarget s (sl :: rest) = syncTarget (s + 1) rest := by
          -- Unfold both marks; the counts differ by the one taker.
          simp [syncTarget, writtenTo, taken, hw, ht]
          -- Plain arithmetic.
          omega
        -- The slot itself is written, and the rest by induction.
        simp [unwrittenUpTo, ht, hw, hh, ih (s + 1)]
      -- It takes none: the mark is the rest's, from the same base.
      · have hh : syncTarget s (sl :: rest) = syncTarget s rest := by
          -- Unfold both marks: the first slot adds no sequence.
          simp [syncTarget, writtenTo, taken, hw, ht]
        -- The slot holds no sequence, and the rest follow by induction.
        simp [unwrittenUpTo, ht, hh, ih s]
    -- It is not: the written prefix is empty and the mark is the base.
    · have hh : syncTarget s (sl :: rest) = s := by
        -- The written prefix stops at the first slot.
        simp [syncTarget, writtenTo, hw, taken]
      -- Read at the base.
      rw [hh]
      -- Does the unwritten slot take a sequence?
      by_cases ht : takes sl.dec = true
      -- Its sequence `s + 1` is above the mark...
      · have h1 : ¬ (s + 1 ≤ s) := by omega
        -- ...so the mark covers neither it nor anything after it.
        simp [unwrittenUpTo, ht, h1, unwritten_at_or_below_base rest (Nat.le_succ s)]
      -- It takes none, and nothing after it is at or below the base.
      · simp [unwrittenUpTo, ht, unwritten_at_or_below_base rest (Nat.le_refl s)]

/-! ## The RED cases -/

/-- Mutant AbortHole: an aborted slot takes a sequence too. The sequences
of the slots that land something, in ring order. -/
def takenSeqsAbortTakes : Nat → List Slot → List Nat
  -- No slot, no sequence.
  | _, [] => []
  -- Look at the first slot.
  | s, sl :: rest =>
    -- A taker takes `s + 1`.
    if takes sl.dec then (s + 1) :: takenSeqsAbortTakes (s + 1) rest
    -- The defect: an abort burns `s + 1` too, which no record will carry.
    else if sl.dec = .abort then takenSeqsAbortTakes (s + 1) rest
    -- A void or discarded slot takes nothing.
    else takenSeqsAbortTakes s rest

/-- A commit, an abort, a commit, all applied, written and durable. -/
def commitAbortCommit : List Slot :=
  -- The three slots, in ring order.
  [⟨.commit, true, true, true⟩, ⟨.abort, true, true, true⟩, ⟨.commit, true, true, true⟩]

/-- **`abort_with_seq_leaves_hole`.** With aborts taking sequences, the two
commits of `commitAbortCommit` hold 1 and 3: sequence 2 is a hole, where
the fixed pipeline gives them 1 and 2. -/
theorem abort_with_seq_leaves_hole :
    -- The claim itself; its proof follows.
    takenSeqsAbortTakes 0 commitAbortCommit = [1, 3] ∧
      (seqs 0 commitAbortCommit).filterMap id = [1, 2] ∧
      takenSeqsAbortTakes 0 commitAbortCommit ≠ List.range' 1 (taken commitAbortCommit) := by
  -- Three slots: compute both numberings.
  decide

/-- Two commits, both applied and written: the first asked for Immediate
and is not durable yet, the second is ready to publish. -/
def laterReady : List Slot := [⟨.commit, true, true, false⟩, ⟨.commit, true, true, true⟩]

/-- **`fetch_max_shows_unpublished`.** The slots of `laterReady` take
sequences 1 and 2, and the frontier is stuck at the first: nothing is
published and the horizon stays at 0. A `fetch_max` horizon raised to 2 by
the second slot's thread shows a reader sequence 1 as well, a commit that
is not durable and that the frontier has not passed. -/
theorem fetch_max_shows_unpublished :
    -- The claim itself; its proof follows.
    seqs 0 laterReady = [some 1, some 2] ∧ frontier laterReady = 0 ∧
      horizon 0 laterReady = 0 ∧ seen 0 0 laterReady = [] ∧
      seen 0 2 laterReady = [1, 2] := by
  -- Two slots: compute each side.
  decide

/-- One commit, written and durable, whose writes are not in the memtable:
what the decided frontier would pass if it did not wait for the apply. -/
def passedUnapplied : List Slot := [⟨.commit, false, true, true⟩]

/-- **`publish_before_apply_hides_commit`.** Without P2 the slot of
`passedUnapplied` is published, the horizon reaches its sequence, and a
reader at the horizon does not find it: its owner cannot read its own
write. With the slot applied the reader sees it. -/
theorem publish_before_apply_hides_commit :
    -- The claim itself; its proof follows.
    frontier passedUnapplied = 1 ∧ horizon 0 passedUnapplied = 1 ∧
      seen 0 1 passedUnapplied = [] ∧
      seen 0 1 [⟨.commit, true, true, true⟩] = [1] := by
  -- One slot: compute each side.
  decide

/-- Three commits: the first and third written, the second not. -/
def gapInLog : List Slot :=
  -- The three slots, in ring order.
  [⟨.commit, true, true, true⟩, ⟨.commit, true, false, true⟩, ⟨.commit, true, true, true⟩]

/-- **`sync_past_gap_covers_unwritten`.** The written prefix of `gapInLog`
ends after the first record, so the sync vouches for sequence 1 and covers
nothing unwritten. A sync that vouched for the last written sequence, 3,
would cover sequence 2, whose record is not in the log. -/
theorem sync_past_gap_covers_unwritten :
    -- The claim itself; its proof follows.
    writtenTo gapInLog = 1 ∧ syncTarget 0 gapInLog = 1 ∧
      unwrittenUpTo 0 (syncTarget 0 gapInLog) gapInLog = [] ∧
      unwrittenUpTo 0 3 gapInLog = [2] := by
  -- Three slots: compute each side.
  decide

-- The end of this file's names.
end Regolith.Pipeline
