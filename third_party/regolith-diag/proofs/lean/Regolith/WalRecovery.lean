/-!
# WalRecovery: the rotation sync (E2) and format 2 replay (plan 4.2)

This file backs the TLA+ model `proofs/tla/WalRotation.tla`, invariants
`RecoversPrefix` (the conjunction of `RecoveryOpens` and `NoGap`) and
`KeepsSynced` (configurations `MC_WalRotation_Green`,
`MC_WalRotation_Red_NoSync` and `MC_WalRotation_Red_NoSync_Gap`).

## The story, for a reader who has never seen the code

Before a write counts, the database notes it in a file, the write-ahead
log, so that after a power cut it can read the log back ("replay") and
rebuild what it had. Writing does not put bytes on the disk; only a "sync"
does. A power cut keeps what was synced and may keep any front part of the
rest, possibly ending in half a record.

Tiny example of what can go wrong (the first part of this file): commits 0
and 1 sit in log file 1, not yet synced. A new file 2 is opened and commit
2 is written and synced there. The power is cut and file 1 loses commit 1.
Now the database either finds half a record in a file that is not the
newest, takes it for damage and refuses to open, or it opens with commits
0 and 2 but not 1. The engine avoids both by syncing a log file before the
next one is created (`RegolithEngine::swap_wal`, src/engine/mod.rs).

The second part of the file is about telling an ordinary torn end of the
log (drop it, and say so) from damage inside what was promised to be on
disk (refuse to open): each record carries a stamp saying how much had
been synced when it was written.

## The rotation sync

The write-ahead log is a sequence of files. Records are appended to the
newest file; a rotation seals it and opens a new one. A power cut keeps,
of each file, the prefix an fsync made durable plus an arbitrary further
prefix of the bytes not yet synced, which may end in the middle of a
record: a torn tail. Replay (`WalReplayIter`, `WalPosition` in
`src/engine/wal_replay.rs`) tolerates a torn tail only in the newest file;
a torn record in an earlier file is damage, and recovery refuses.

What is proved, in plain words:

1. `step_inv` and `reachable_inv`: when a rotation syncs the sealed file
   before the new file exists, every sealed file is fully durable. That is
   the protocol's invariant.
2. `recovers_prefix`: then, after a power cut at any moment, whatever each
   file kept, recovery succeeds and yields commits `0, 1, ..., m - 1` for
   some `m`: a gap-free prefix of commit order, containing every record a
   sync made durable.
3. `no_sync_breaks_recovery`: a rotation that does not sync lets a power
   cut either make recovery refuse (a torn record in a sealed file) or
   recover commit 2 without commits 0 and 1 (a gap). This is the RED
   configuration as a concrete counterexample.

The order inside the rotation is the one the model checks: the sync of the
sealed file completes before the new file is created. A new file that
exists, even empty, beside an unsynced sealed file makes that sealed file
an earlier file, whose torn tail replay refuses.

The second part of this file, "Format 2", backs `proofs/tla/WalRecovery.tla`
(configurations `MC_WalRecovery_*`): the `synced_through` stamps, CLOSE,
and the replay rule that refuses below P and drops a tail above it. Its
own header lists what it proves.
-/

-- Everything below is named Regolith.WalRecovery.<name>.
namespace Regolith.WalRecovery

/-- One write-ahead log file. -/
structure LogFile where
  /-- The records the file holds, in append order. A record is named by
  its commit number; commits are numbered `0, 1, 2, ...` in commit order. -/
  records : List Nat
  /-- How many leading records an fsync has made durable. -/
  synced : Nat
  -- Lean may compare two log files for equality, so the counterexample at the end of this part can
  -- be computed.
  deriving DecidableEq

/-- The whole log. -/
structure Wal where
  /-- The sealed files, oldest first: earlier rotations closed them. -/
  sealed : List LogFile
  /-- The newest file: the one taking records. -/
  active : LogFile
  /-- The number the next commit takes. -/
  next : Nat
  -- The same for whole logs.
  deriving DecidableEq

/-- Every record in the log, oldest file first. -/
def allRecords (w : Wal) : List Nat := (w.sealed.map LogFile.records).flatten ++ w.active.records

/-- How many records are durable: every record of a sealed file (the
invariant makes each one fully synced) plus the synced prefix of the newest
file. -/
def durable (w : Wal) : Nat := ((w.sealed.map LogFile.records).flatten).length + w.active.synced

/-- The protocol's invariant. -/
structure Inv (w : Wal) : Prop where
  /-- The files hold commits `0, 1, ..., next - 1`, in order, each once. -/
  commit_order : allRecords w = List.range w.next
  /-- Every sealed file is fully durable. -/
  sealed_synced : ∀ f ∈ w.sealed, f.synced = f.records.length
  /-- The newest file's synced prefix is within the file. -/
  active_synced : w.active.synced ≤ w.active.records.length

/-- The start: one empty file, no commit yet. -/
def init : Wal := ⟨[], ⟨[], 0⟩, 0⟩

/-- The fixed protocol's steps. -/
inductive Step : Wal → Wal → Prop
  /-- A commit appends its record to the newest file. -/
  | append (w : Wal) :
      -- The newest file gains the record of commit `next` at its end,
      Step w { w with active := { w.active with records := w.active.records ++ [w.next] },
                      -- and the next commit number moves up by one.
                      next := w.next + 1 }
  /-- An fsync of the newest file makes every record in it durable. -/
  | sync (w : Wal) :
      -- The newest file's synced count becomes its number of records.
      Step w { w with active := { w.active with synced := w.active.records.length } }
  /-- The fix: a rotation first syncs the newest file, then seals it and
  opens a new, empty one. -/
  | rotate (w : Wal) :
      -- The newest file, marked fully synced, joins the end of the sealed files,
      Step w { w with sealed := w.sealed ++ [{ w.active with synced := w.active.records.length }],
                      -- and a new, empty file becomes the newest.
                      active := ⟨[], 0⟩ }

/-- Any number of steps, one after another. -/
inductive Steps : Wal → Wal → Prop
  /-- No step at all. -/
  | refl (w : Wal) : Steps w w
  /-- Some steps, then one more. -/
  | tail {u v w : Wal} : Steps u v → Step v w → Steps u w

/-- The start satisfies the invariant. -/
theorem init_inv : Inv init :=
  -- No records, no sealed file, nothing synced.
  ⟨rfl, fun _ h => absurd h List.not_mem_nil, Nat.le_refl 0⟩

/-- **Every step keeps the invariant.** -/
theorem step_inv {w w' : Wal}
    -- The invariant holds before the step.
    (hinv : Inv w)
    -- One step of the fixed protocol.
    (hstep : Step w w') :
    -- The statement: the invariant holds after the step.
    Inv w' := by
  -- Take the invariant's three facts: the commit order, the sealed files fully synced, the newest
  -- file's synced count within it.
  obtain ⟨horder, hsealed, hactive⟩ := hinv
  -- Split on the kind of step.
  cases hstep with
  -- Case: an append.
  | append =>
    -- The sealed files are untouched; the commit order and the synced bound are left to show.
    refine ⟨?_, hsealed, ?_⟩
    -- Appending commit `next` extends `0, ..., next - 1` by `next`.
    ·
      -- Unfold "all records" in the assumption and the goal.
      unfold allRecords at horder ⊢
      -- Simplify the record fields after the step.
      simp only
      -- Regroup the lists; the old records are 0, ..., next - 1, and one more makes 0, ..., next.
      rw [← List.append_assoc, horder, List.range_succ]
    -- The synced prefix stays within the longer file.
    ·
      -- The file is one record longer.
      simp only [List.length_append, List.length_singleton]
      -- The synced count was within the shorter file: arithmetic.
      omega
  -- Case: a sync.
  | sync =>
    -- The records do not change; the newest file is now fully synced.
    exact ⟨horder, hsealed, Nat.le_refl _⟩
  -- Case: a rotation.
  | rotate =>
    -- The new newest file is empty with nothing synced (0 ≤ 0); the commit order and the sealed
    -- files are left to show.
    refine ⟨?_, ?_, Nat.le_refl 0⟩
    -- The sealed file's records move to the sealed list unchanged.
    ·
      -- Unfold "all records" in the assumption and the goal.
      unfold allRecords at horder ⊢
      -- Join the lists: the old sealed records, then the sealed newest file's, then nothing.
      simp only [List.map_append, List.map_cons, List.map_nil, List.flatten_append,
        List.flatten_cons, List.flatten_nil, List.append_nil]
      -- That is the same list as before.
      exact horder
    -- The new sealed file was synced by the rotation; the older ones already were.
    ·
      -- Take a sealed file after the step.
      intro f hf
      -- It is an older sealed file, or the newly sealed one.
      rcases List.mem_append.mp hf with hold | hnew
      -- An older one was fully synced already.
      · exact hsealed f hold
      -- The newly sealed one:
      · rcases List.mem_singleton.mp hnew with rfl
        -- the rotation set its synced count to its length.
        rfl

/-- Every state the fixed protocol reaches satisfies the invariant. -/
theorem reachable_inv {w : Wal} (hreach : Steps init w) : Inv w := by
  -- Go through the steps one at a time.
  induction hreach with
  -- No step: the start satisfies the invariant.
  | refl => exact init_inv
  -- Some steps, then one more: the invariant held before the last step (the claim so far), and a
  -- step keeps it.
  | tail _ hstep ih => exact step_inv ih hstep

/-! ## Power cuts and replay -/

/-- What a power cut leaves of one file. -/
structure Cut where
  /-- How many whole records survive. -/
  kept : Nat
  /-- Whether part of one more record follows them: a torn tail. -/
  torn : Bool

/-- `Survives f c`: a power cut may leave `c` of file `f`. Everything synced
survives, at most every record survives, and a torn partial record can only
follow when at least one more record was written. -/
def Survives (f : LogFile) (c : Cut) : Prop :=
  -- Everything synced is kept; no more than all records are kept; and a torn record follows only
  -- when fewer than all were kept.
  f.synced ≤ c.kept ∧ c.kept ≤ f.records.length ∧ (c.torn = true → c.kept < f.records.length)

/-- Replay after a power cut. The sealed files come first, oldest first,
each with what the cut left of it; then the newest file. A sealed file
with a torn tail makes recovery refuse (`none`). The newest file's torn
tail ends the log. Otherwise recovery yields every surviving record, in
file order. -/
def replay : List (LogFile × Cut) → LogFile × Cut → Option (List Nat)
  -- The newest file: its whole surviving records, whatever follows them.
  | [], (f, c) => some (f.records.take c.kept)
  -- A sealed file: refuse if torn, else its records and then the rest.
  | (f, c) :: rest, newest =>
    -- A torn sealed file refuses; otherwise its kept records are put in front of what the rest
    -- yields.
    if c.torn then none else (replay rest newest).map (f.records.take c.kept ++ ·)

/-- When every sealed file is fully synced, no power cut tears one, so
replay succeeds and yields all of their records, then what survived of the
newest file. -/
theorem replay_synced {newest : LogFile × Cut} :
    -- The statement: for every list of sealed files with their cuts, if each file is fully synced
    -- and each cut is one a power cut may leave, replay yields all their records and then what
    -- survived of the newest file.
    ∀ (cs : List (LogFile × Cut)),
      -- Every sealed file is fully synced ...
      (∀ p ∈ cs, p.1.synced = p.1.records.length) →
      -- ... and the cut left it something it may leave.
      (∀ p ∈ cs, Survives p.1 p.2) →
      replay cs newest =
        some (((cs.map Prod.fst).map LogFile.records).flatten ++
          newest.1.records.take newest.2.kept)
  -- Case: no sealed file.
  | [], _, _ => by
    -- Only the newest file.
    obtain ⟨f, c⟩ := newest
    -- Unfold replay: the newest file's kept records.
    simp [replay]
  -- Case: a first sealed file f with cut c, then the rest.
  | (f, c) :: rest, hsync, hsurv => by
    -- The first sealed file is fully synced, so the cut kept all of it and
    -- could not tear it.
    have hs := hsync (f, c) List.mem_cons_self
    -- The cut is one a power cut may leave: three bounds.
    obtain ⟨hlo, hhi, htorn⟩ := hsurv (f, c) List.mem_cons_self
    -- Simplify what "the file of the pair" and "the cut of the pair" are.
    simp only at hs hlo hhi htorn
    -- Kept is between the synced count, which is the whole length, and the whole length: so it is
    -- the whole length.
    have hkept : c.kept = f.records.length := by omega
    -- The cut did not tear the file:
    have hnottorn : c.torn = false := by
      -- split on the torn flag.
      cases ht : c.torn
      -- Not torn: that is the claim.
      · rfl
      -- Torn: then fewer than all records were kept,
      · have := htorn ht
        -- against "all were kept": arithmetic says no.
        omega
    -- The rest replays by induction.
    have ih := replay_synced (newest := newest) rest
      -- (the rest of the sealed files are fully synced as well,
      (fun p hp => hsync p (List.mem_cons_of_mem _ hp))
      -- and their cuts are ones a power cut may leave)
      (fun p hp => hsurv p (List.mem_cons_of_mem _ hp))
    -- Replay does not refuse the untorn file, yields all of its records
    -- (`take` of the whole length is the whole list), then the rest.
    simp only [replay, hnottorn, Bool.false_eq_true, ite_false, ih, Option.map_some,
      List.map_cons, List.flatten_cons, hkept, List.take_length, List.append_assoc]

/-- **E2, recovery yields a gap-free prefix.** Take any state the fixed
protocol satisfies the invariant in, and any power cut: each sealed file
and the newest file keep something a cut may leave. Recovery then succeeds
and yields commits `0, 1, ..., m - 1` for some `m` no larger than the
number of commits, and `m` covers every durable record. -/
theorem recovers_prefix {w : Wal}
    -- The invariant holds.
    (hinv : Inv w)
    -- What the cut left of each sealed file, in order ...
    (cs : List (LogFile × Cut)) (hcs : cs.map Prod.fst = w.sealed)
    -- ... each something a cut may leave ...
    (hsurv : ∀ p ∈ cs, Survives p.1 p.2)
    -- ... and what it left of the newest file.
    (c : Cut) (hc : Survives w.active c) :
    -- The statement: for some m, replay yields exactly commits 0, ..., m - 1; m is at most the
    -- number of commits; and every durable record is among them.
    ∃ m, replay cs (w.active, c) = some (List.range m) ∧ m ≤ w.next ∧ durable w ≤ m := by
  -- horder: the log holds commits `0, ..., next - 1`; hsealed: every sealed
  -- file is fully synced. hlo, hhi: the newest file kept between its synced
  -- prefix and all of its records.
  obtain ⟨horder, hsealed, -⟩ := hinv
  -- Take the bounds on what the newest file kept.
  obtain ⟨hlo, hhi, -⟩ := hc
  -- The sealed files are fully synced, so replay is all of their records
  -- followed by what survived of the newest file.
  have hsync : ∀ p ∈ cs, p.1.synced = p.1.records.length := by
    -- Take a sealed file p with its cut.
    intro p hp
    -- It is enough that p's file is one of the log's sealed files.
    apply hsealed
    -- The files of the pairs are the sealed files.
    rw [← hcs]
    -- And p is one of the pairs.
    exact List.mem_map_of_mem hp
  -- So replay is the sealed files' records followed by what survived of the newest file.
  rw [replay_synced cs hsync hsurv, hcs]
  -- Name the sealed files' records `F`; with the newest file's they are
  -- `0, ..., next - 1`.
  generalize hF : (w.sealed.map LogFile.records).flatten = F at horder
  -- Unfold "all records" in the commit-order fact.
  unfold allRecords at horder
  -- Write it with F.
  rw [hF] at horder
  -- Counting: the sealed records and the newest file's make `next`.
  have hlen : F.length + w.active.records.length = w.next := by
    -- Equal lists have equal lengths.
    have := congrArg List.length horder
    -- Compute both lengths.
    simpa using this
  -- `F` followed by the first `kept` records of the newest file is the
  -- first `F.length + kept` records of the whole log, hence a range. That
  -- length is at most `next`, since `kept` is at most the newest file's.
  refine ⟨F.length + c.kept, ?_, by omega, ?_⟩
  -- Taking `F.length + kept` records of `F ++ newest` takes all of `F` and then `kept` records of
  -- the newest file.
  ·
    -- The claim, as an equation between lists.
    have htake : F ++ w.active.records.take c.kept =
        (F ++ w.active.records).take (F.length + c.kept) := by
      -- Split "the first n of a joined list" into the first part's share and the second's;
      rw [List.take_append,
        -- the first part, F, is taken whole.
        List.take_of_length_le (l := F) (i := F.length + c.kept) (by omega)]
      -- What is left over for the newest file is `kept`.
      simp
    -- So replay yields the first `F.length + kept` of `0, ..., next - 1`,
    -- which is `0, ..., F.length + kept - 1` (`List.take_range`).
    simp only [htake, horder, List.take_range]
    -- Both sides are ranges from 0; compare their lengths.
    congr 2
    -- Arithmetic.
    omega
  -- Everything durable survived: the sealed records all, and the newest file's synced prefix, since
  -- `kept` is at least that.
  ·
    -- Unfold "durable".
    unfold durable
    -- Write it with F.
    rw [hF]
    -- The synced count is at most `kept`: arithmetic.
    omega

/-- **E2 for every reachable state.** -/
theorem reachable_recovers_prefix {w : Wal}
    -- `w` is reachable from the start by the fixed protocol.
    (hreach : Steps init w)
    -- (what the cut left of each sealed file,
    (cs : List (LogFile × Cut)) (hcs : cs.map Prod.fst = w.sealed)
    -- each something a cut may leave,
    (hsurv : ∀ p ∈ cs, Survives p.1 p.2)
    -- and what it left of the newest file)
    (c : Cut) (hc : Survives w.active c) :
    -- The statement: recovery yields commits 0, ..., m - 1 for some m covering every durable
    -- record.
    ∃ m, replay cs (w.active, c) = some (List.range m) ∧ m ≤ w.next ∧ durable w ≤ m :=
  -- A reachable state satisfies the invariant, and the theorem above applies.
  recovers_prefix (reachable_inv hreach) cs hcs hsurv c hc

/-! ## The RED case: rotation without a sync -/

/-- The defective protocol: a rotation seals the newest file as it is,
synced or not. -/
inductive StepNoSync : Wal → Wal → Prop
  /-- As `Step.append`. -/
  | append (w : Wal) :
      -- The newest file gains the record of commit `next`,
      StepNoSync w { w with active := { w.active with records := w.active.records ++ [w.next] },
                            -- and the next commit number moves up.
                            next := w.next + 1 }
  /-- As `Step.sync`. -/
  | sync (w : Wal) :
      -- The newest file's synced count becomes its number of records.
      StepNoSync w { w with active := { w.active with synced := w.active.records.length } }
  /-- The defect: no sync before the new file opens. -/
  | rotate (w : Wal) :
      -- The newest file joins the sealed files as it is, synced or not, and a new, empty file
      -- becomes the newest.
      StepNoSync w { w with sealed := w.sealed ++ [w.active], active := ⟨[], 0⟩ }

/-- Any number of defective steps. -/
inductive StepsNoSync : Wal → Wal → Prop
  /-- No step at all. -/
  | refl (w : Wal) : StepsNoSync w w
  /-- Some steps, then one more. -/
  | tail {u v w : Wal} : StepsNoSync u v → StepNoSync v w → StepsNoSync u w

/-- Commits 0 and 1 in a sealed file that was never synced, and commit 2
in the newest file. -/
def unsyncedRotation : Wal := ⟨[⟨[0, 1], 0⟩], ⟨[2], 0⟩, 3⟩

/-- **`no_sync_breaks_recovery`.** The defective protocol reaches
`unsyncedRotation` from the start: two appends, a rotation without a sync,
one more append. Then two power cuts break recovery:

* the sealed file keeps commit 0 and half of commit 1: a torn record in a
  sealed file, and recovery refuses;
* the sealed file keeps nothing and the newest file keeps commit 2:
  recovery yields commit 2 alone, which is no prefix of commit order. -/
theorem no_sync_breaks_recovery :
    -- The statement, in seven parts: the defective protocol reaches the state "unsyncedRotation";
    -- the torn cut is possible and makes replay refuse; the clean cut is possible and makes replay
    -- yield commit 2 alone; and [2] is not commits 0 to m - 1 for any m.
    StepsNoSync init unsyncedRotation ∧
    -- The torn cut is one a power cut may leave, and replay refuses.
    Survives ⟨[0, 1], 0⟩ ⟨1, true⟩ ∧
    replay [(⟨[0, 1], 0⟩, ⟨1, true⟩)] (⟨[2], 0⟩, ⟨1, false⟩) = none ∧
    -- The clean cut is one a power cut may leave, and replay leaves a gap.
    Survives ⟨[0, 1], 0⟩ ⟨0, false⟩ ∧ Survives ⟨[2], 0⟩ ⟨1, false⟩ ∧
    replay [(⟨[0, 1], 0⟩, ⟨0, false⟩)] (⟨[2], 0⟩, ⟨1, false⟩) = some [2] ∧
    ∀ m, [2] ≠ List.range m := by
  -- Seven parts; the two replays are computed, the others follow.
  refine ⟨?_, ?_, by decide, ?_, ?_, by decide, ?_⟩
  -- The four defective steps, in order.
  ·
    -- From the start: append, append, rotate without a sync, append.
    exact .tail (.tail (.tail (.tail (.refl _) (.append _)) (.append _)) (.rotate _)) (.append _)
  -- Nothing synced, one of two records kept, a torn second one.
  ·
    -- The three bounds of "may be left by a power cut", each computed.
    exact ⟨by decide, by decide, fun _ => by decide⟩
  -- Nothing synced, nothing kept, no torn record.
  ·
    -- The three bounds, each computed; "torn" is false, so its bound holds for lack of a case.
    exact ⟨by decide, by decide, fun h => absurd h (by decide)⟩
  -- Nothing synced, the one record kept, no torn record.
  ·
    -- The three bounds, each computed.
    exact ⟨by decide, by decide, fun h => absurd h (by decide)⟩
  -- A range of length 1 is `[0]`, not `[2]`.
  ·
    -- Suppose [2] were the range of some length m.
    intro m h
    -- Then the two lists have equal lengths.
    have hlen := congrArg List.length h
    -- So m is 1.
    simp only [List.length_cons, List.length_nil, List.length_range] at hlen
    -- Replace m by 1.
    subst hlen
    -- But the range of length 1 is [0], not [2].
    simp at h

/-! ## Format 2: `synced_through` stamps, CLOSE, and the replay rule (plan 4.2)

This part backs `proofs/tla/WalRecovery.tla`, invariants `RecoveryOpens`,
`NoGap`, `KeepsSynced`, `AckedSurvive`, `NoProvenLoss`, `LossIsReported`
and `WrongKeyRefuses` (configurations `MC_WalRecovery_*`).

Format 2 stamps every group record with `synced_through`: how many leading
records the last completed sync had made durable when the record was
written. A clean close syncs every record, appends CLOSE and syncs again.
Replay of the newest log reads P, the largest stamp of any usable record,
or the whole file when a usable CLOSE ends it, and finds O, the first
unusable record. It refuses when O < P, since a surviving record proves
the damaged one was synced; otherwise it keeps the records before O and
drops the rest, reporting it.

Positions and stamps count records here, not bytes: record `i` starts
where `i` records end, so "O < P in bytes" is "O < P in records".

The earlier logs are complete by the rotation sync (the first part of this
file), so here they are a list of commits `0, 1, ..., e - 1`, all synced;
the newest log's group `i` holds commit `e + i`. One record per group,
holding one commit: a group of several is lost or kept whole, like one.

What is proved, in plain words:

1. `step2_inv`, `reachable2_inv`: the writer keeps every stamp at or below
   the completed sync, and appends CLOSE only after a sync of every group.
2. `honest_of_inv`: so under any crash that keeps the synced prefix (the
   device honoured its flush), no surviving record claims more than
   survived: P is at most O.
3. `replay2_opens_of_honest` and `recovers_prefix2`: then replay opens,
   and recovery is a gap-free prefix of commit order that keeps every
   group a completed sync covered.
4. `replay2_keeps_proven`: whenever replay opens, it keeps every group
   below P: no record a surviving stamp proves synced is discarded.
5. `residual_is_reported`: when damage reached the synced prefix (bit rot,
   or a torn write over a sector holding acknowledged bytes; D6 leaves
   that case open) and replay still opens, it has discarded a nonempty
   tail, which it reports: the loss is never silent.
6. `wrong_key_refuses`: with encryption at rest the stamp is sealed under
   the key, so a wrong key refuses instead of dropping the log.
7. The RED cases as counterexamples: `drop_below_p_loses_proven`,
   `refuse_above_p_never_opens`, `format1_refuses_torn_tail` and
   `unsealed_stamp_drops_log`.
-/

/-- The newest log in format 2: its group records, then CLOSE when it was
closed cleanly. -/
structure Log2 where
  /-- Each group record's `synced_through` stamp, in order. -/
  stamps : List Nat
  /-- How many leading records a completed sync made durable. CLOSE, when
  present, is the record after the groups. -/
  synced : Nat
  /-- Whether CLOSE follows the groups. -/
  closed : Bool

/-- How many records the newest log holds: the groups, and CLOSE. -/
def Log2.total (l : Log2) : Nat := l.stamps.length + (if l.closed then 1 else 0)

/-- The whole write-ahead log in format 2. -/
structure Wal2 where
  /-- The commits of the earlier logs, in order. -/
  earlier : List Nat
  /-- The newest log. -/
  log : Log2

/-- O: how many leading records are usable, which is the position of the
first unusable one. `ok` says, for each record the crash left in the file,
whether it is usable (it survived and its checksum or tag verifies). -/
def firstBad : List Bool → Nat
  -- No record: 0.
  | [] => 0
  -- A usable record first: one more than the count for the rest.
  | true :: rest => firstBad rest + 1
  -- An unusable record first: 0.
  | false :: _ => 0

/-- The largest stamp among the usable group records. -/
def groupProof : List Nat → List Bool → Nat
  -- A stamp with its record: the larger of this stamp (counted only if its record is usable) and
  -- the answer for the rest.
  | st :: sts, ok :: oks => max (if ok then st else 0) (groupProof sts oks)
  -- No more stamps, or no more records in the file: 0.
  | _, _ => 0

/-- P: what the surviving records prove synced. The largest stamp of a
usable group record, or the whole file when CLOSE survived usable. -/
def proofOf (l : Log2) (ok : List Bool) : Nat :=
  -- The larger of: the largest usable stamp;
  max (groupProof l.stamps ok)
    -- and, when the log was closed and the record right after the groups (CLOSE) is usable, the
    -- whole file.
    (if l.closed && ok[l.stamps.length]? == some true then l.stamps.length + 1 else 0)

/-- Replay of the newest log in format 2. `stampOk` says whether the log's
stamp verifies (with encryption, whether the key is right). The result is
how many group records recovery keeps, or `none` when it refuses. -/
def replay2 (stampOk : Bool) (l : Log2) (ok : List Bool) : Option Nat :=
  -- A stamp that fails: refuse.
  if !stampOk then none
  -- The first unusable record lies below what is proven synced: refuse.
  else if firstBad ok < proofOf l ok then none
  -- Otherwise keep the groups before the first unusable record (never more than there are groups).
  else some (min (firstBad ok) l.stamps.length)

/-- What recovery yields when it keeps `kept` groups of the newest log:
the earlier logs' commits, then the newest log's first `kept`. -/
def recovered2 (w : Wal2) (kept : Nat) : List Nat :=
  -- The earlier commits, then the next `kept` commit numbers.
  w.earlier ++ List.range' w.earlier.length kept

/-- `Survives2 l ok`: a crash the device's flush honoured may leave `ok`.
Every record a completed sync covered survived usable, and the file holds
no more records than were written. -/
def Survives2 (l : Log2) (ok : List Bool) : Prop :=
  -- The first unusable record is at or past the synced count, and the file holds no more records
  -- than were written.
  l.synced ≤ firstBad ok ∧ ok.length ≤ l.total

/-- The writer's invariant on the newest log. -/
structure Inv2Log (l : Log2) : Prop where
  /-- No stamp claims more than the completed sync. -/
  stamps_le : ∀ st ∈ l.stamps, st ≤ l.synced
  /-- The sync covered only records that exist. -/
  synced_le : l.synced ≤ l.total
  /-- CLOSE was appended only after every group was synced. -/
  close_after_sync : l.closed = true → l.stamps.length ≤ l.synced

/-- The invariant of the whole log: the earlier logs hold commits
`0, ..., e - 1`, and the newest log keeps `Inv2Log`. -/
structure Inv2 (w : Wal2) : Prop where
  /-- The earlier logs hold commits `0, ..., e - 1` in order. -/
  earlier_order : w.earlier = List.range w.earlier.length
  /-- The newest log keeps the writer's invariant. -/
  log_inv : Inv2Log w.log

/-- The start: no earlier log, an empty newest log. -/
def init2 : Wal2 := ⟨[], ⟨[], 0, false⟩⟩

/-- The format 2 writer's steps. -/
inductive Step2 : Wal2 → Wal2 → Prop
  /-- A commit group appends its record, stamped with the completed sync. -/
  | append (w : Wal2) (hopen : w.log.closed = false) :
      -- The newest log gains one stamp: the current synced count.
      Step2 w { w with log := { w.log with stamps := w.log.stamps ++ [w.log.synced] } }
  /-- A sync that began when `k` records were written completes. -/
  | sync (w : Wal2) (k : Nat) (hgrow : w.log.synced < k) (hle : k ≤ w.log.total) :
      -- The synced count becomes k.
      Step2 w { w with log := { w.log with synced := k } }
  /-- A clean close appends CLOSE once every group is synced; a later
  `sync` makes CLOSE durable. -/
  | close (w : Wal2) (hopen : w.log.closed = false) (hall : w.log.synced = w.log.stamps.length) :
      -- The log is marked closed.
      Step2 w { w with log := { w.log with closed := true } }
  /-- A rotation syncs the newest log (E2), so its commits join the
  earlier logs, and a new, empty log takes the next record. -/
  | rotate (w : Wal2) (hopen : w.log.closed = false) :
      -- The earlier commits gain the newest log's commits, and a new, empty, open log takes over.
      Step2 w ⟨w.earlier ++ List.range' w.earlier.length w.log.stamps.length, ⟨[], 0, false⟩⟩

/-- Any number of format 2 steps. -/
inductive Steps2 : Wal2 → Wal2 → Prop
  /-- No step at all. -/
  | refl (w : Wal2) : Steps2 w w
  /-- Some steps, then one more. -/
  | tail {u v w : Wal2} : Steps2 u v → Step2 v w → Steps2 u w

/-- The start satisfies the invariant. -/
theorem init2_inv : Inv2 init2 :=
  -- No commit, no stamp, nothing synced, not closed.
  ⟨rfl, ⟨fun _ h => absurd h List.not_mem_nil, Nat.le_refl 0, fun h => absurd h (by decide)⟩⟩

/-- **Every format 2 step keeps the invariant.** -/
theorem step2_inv {w w' : Wal2}
    -- The invariant holds before the step.
    (hinv : Inv2 w)
    -- One step of the writer.
    (hstep : Step2 w w') :
    -- The statement: the invariant holds after the step.
    Inv2 w' := by
  -- Take the invariant apart: the earlier commits' order, and the three facts about the newest log.
  obtain ⟨horder, ⟨hst, hsyn, hclose⟩⟩ := hinv
  -- Split on the kind of step.
  cases hstep with
  -- Case: an append.
  | append hopen =>
    -- The earlier commits are untouched; three facts about the log are left. (`dsimp only`
    -- simplifies the record fields after the step in each.)
    refine ⟨horder, ⟨?_, ?_, ?_⟩⟩ <;> dsimp only
    -- The new stamp is the completed sync; the old ones were below it.
    ·
      -- Take a stamp of the log after the step.
      intro st hmem
      -- It is an old stamp or the new one.
      rcases List.mem_append.mp hmem with h | h
      -- An old one was at most the synced count.
      · exact hst st h
      -- The new one is the synced count itself,
      · rw [List.mem_singleton.mp h]
        -- so it is at most it.
        exact Nat.le_refl _
    -- One more record: the sync still covers only records that exist.
    ·
      -- Unfold "total": the number of stamps (one more now) plus one for CLOSE.
      simp only [Log2.total, List.length_append, List.length_singleton] at hsyn ⊢
      -- The synced count was within the smaller total: arithmetic.
      omega
    -- The log is still open.
    ·
      -- Suppose it were closed.
      intro h
      -- It is open, by the step's condition.
      rw [hopen] at h
      -- "false = true": impossible.
      cases h
  -- Case: a sync up to k.
  | sync k hgrow hle =>
    -- The earlier commits are untouched and k is within the total (the step's condition); two facts
    -- are left.
    refine ⟨horder, ⟨?_, hle, ?_⟩⟩ <;> dsimp only
    -- The sync only grew: every stamp is still below it.
    ·
      -- Take a stamp.
      intro st hmem
      -- It was at most the old synced count,
      have := hst st hmem
      -- which is below k.
      omega
    -- If closed, every group was synced before, and still is.
    ·
      -- Suppose the log is closed.
      intro h
      -- Then every group was within the old synced count,
      have := hclose h
      -- which is below k.
      omega
  -- Case: a clean close.
  | close hopen hall =>
    -- The earlier commits and the stamps are untouched, and every group is synced (the step's
    -- condition); the bound on the synced count is left.
    refine ⟨horder, ⟨hst, ?_, fun _ => Nat.le_of_eq hall.symm⟩⟩
    -- CLOSE adds a record after every synced group.
    -- Unfold "total" before and after: the log was open and is closed now.
    simp only [Log2.total, hopen] at hsyn ⊢
    -- Compute the two "one for CLOSE" terms: 0 before, 1 after.
    simp only [Bool.false_eq_true, ite_false, ite_true] at hsyn ⊢
    -- The bound only got looser: arithmetic.
    omega
  -- Case: a rotation.
  | rotate hopen =>
    -- The new log has no stamp and nothing synced; two facts are left: the earlier commits' order,
    -- and "closed only after a sync".
    refine ⟨?_, ⟨fun _ h => absurd h List.not_mem_nil, Nat.le_refl 0, ?_⟩⟩ <;> dsimp only
    -- The newest log's commits continue the earlier ones.
    ·
      -- The new earlier list is as long as the old one plus the number of groups.
      simp only [List.length_append, List.length_range']
      -- The old list is 0, ..., e - 1; adding the next numbers gives 0, ..., e + groups - 1.
      rw [horder, List.length_range, List.range_add, List.range'_eq_map_range]
    -- The new log is open.
    ·
      -- Suppose it were closed.
      intro h
      -- "false = true": impossible.
      cases h

/-- Every state the format 2 writer reaches satisfies the invariant. -/
theorem reachable2_inv {w : Wal2} (hreach : Steps2 init2 w) : Inv2 w := by
  -- Go through the steps one at a time.
  induction hreach with
  -- No step: the start satisfies the invariant.
  | refl => exact init2_inv
  -- Some steps, then one more: a step keeps the invariant.
  | tail _ hstep ih => exact step2_inv ih hstep

/-- O is at most the number of records in the file. -/
theorem firstBad_le_length : ∀ ok : List Bool, firstBad ok ≤ ok.length
  -- No record: 0 is at most 0.
  | [] => Nat.le_refl 0
  -- A usable record first.
  | true :: rest => by
    -- The claim for the rest.
    have := firstBad_le_length rest
    -- Unfold: one more on each side.
    simp only [firstBad, List.length_cons]
    -- Arithmetic.
    omega
  -- An unusable record first: 0 is at most anything.
  | false :: _ => Nat.zero_le _

/-- If the first `g` records are usable and record `g` is too, the first
`g + 1` are. -/
theorem firstBad_succ : ∀ (ok : List Bool) (g : Nat), g ≤ firstBad ok → ok[g]? = some true →
    g + 1 ≤ firstBad ok
  -- No record in the file: record g cannot be usable there.
  | [], _, _, h => by simp at h
  -- A usable record first, and g is 0: the count is at least 1.
  | true :: rest, 0, _, _ => by simp only [firstBad]; omega
  -- A usable record first, and g is one more than some g'.
  | true :: rest, g + 1, hle, hg => by
    -- Drop the first record: the claim for the rest, shifted by one.
    simp only [firstBad] at hle ⊢
    -- The claim for the rest, at g'.
    have := firstBad_succ rest g (by omega) (by simpa using hg)
    -- One more on each side: arithmetic.
    omega
  -- An unusable record first, and g is 0: record 0 is not usable, against the assumption.
  | false :: _, 0, _, hg => by simp at hg
  -- An unusable record first: the count is 0, so g cannot be at least 1.
  | false :: _, g + 1, hle, _ => by simp only [firstBad] at hle; omega

/-- The largest usable stamp is at most any bound on every stamp. -/
theorem groupProof_le {n : Nat} : ∀ (sts : List Nat) (oks : List Bool),
    (∀ st ∈ sts, st ≤ n) → groupProof sts oks ≤ n
  -- No stamp: 0.
  | [], _, _ => Nat.zero_le _
  -- No record in the file: 0.
  | _ :: _, [], _ => Nat.zero_le _
  -- A stamp with its record, then the rest.
  | st :: sts, ok :: oks, h => by
    -- This stamp is at most n.
    have h1 := h st List.mem_cons_self
    -- The largest usable stamp of the rest is at most n (the claim for the rest).
    have h2 := groupProof_le sts oks fun x hx => h x (List.mem_cons_of_mem _ hx)
    -- Unfold: the larger of this stamp (or 0) and the rest's.
    simp only [groupProof]
    -- Whether this record is usable or not, both candidates are at most n.
    split <;> omega

/-- **The stamps are honest.** Under the writer's invariant, after a crash
the device's flush honoured, no usable record proves more than survived:
P is at most O. -/
theorem honest_of_inv {l : Log2} {ok : List Bool}
    -- The writer's invariant.
    (hinv : Inv2Log l)
    -- A crash that kept the synced prefix.
    (hsurv : Survives2 l ok) :
    -- The statement: P is at most O.
    proofOf l ok ≤ firstBad ok := by
  -- Take from the invariant: the stamps are at most the synced count, and CLOSE came only after a
  -- sync.
  obtain ⟨hst, -, hclose⟩ := hinv
  -- Take from the crash: the first unusable record is at or past the synced count.
  obtain ⟨hsyn, -⟩ := hsurv
  -- Unfold P: the larger of two candidates.
  unfold proofOf
  -- A maximum is at most O when both candidates are.
  refine Nat.max_le.mpr ⟨?_, ?_⟩
  -- Every stamp is at most the completed sync, which survived.
  ·
    -- The largest usable stamp is at most the synced count, which is at most O.
    exact Nat.le_trans (groupProof_le _ _ hst) hsyn
  -- A usable CLOSE: every group before it was synced, so it survived, and CLOSE itself is usable.
  ·
    -- Split on whether a usable CLOSE counts.
    split
    -- It does; name that fact h.
    · rename_i h
      -- Spell it out: the log is closed, and the record after the groups is usable.
      simp only [Bool.and_eq_true, beq_iff_eq] at h
      -- Take the two parts.
      obtain ⟨hc, hg⟩ := h
      -- All the groups are within the synced count and so before O, and the record after them is
      -- usable: O is past it.
      exact firstBad_succ ok _ (Nat.le_trans (hclose hc) hsyn) hg
    -- It does not: the candidate is 0.
    · exact Nat.zero_le _

/-- **Replay opens on honest stamps.** When the stamp verifies and P is at
most O, replay keeps every group before O. -/
theorem replay2_opens_of_honest {l : Log2} {ok : List Bool}
    -- P is at most O.
    (h : proofOf l ok ≤ firstBad ok) :
    -- The statement: replay opens and keeps the groups before O.
    replay2 true l ok = some (min (firstBad ok) l.stamps.length) := by
  -- P at most O means O is not below P.
  have hnot : ¬ firstBad ok < proofOf l ok := by omega
  -- Unfold replay: the stamp verifies and the refusal does not fire.
  simp only [replay2, Bool.not_true, Bool.false_eq_true, ite_false, hnot]

/-- **Replay never discards what a stamp proves synced.** Whenever replay
opens, it keeps every group below P. -/
theorem replay2_keeps_proven {s : Bool} {l : Log2} {ok : List Bool} {kept : Nat}
    -- Replay opened, keeping `kept` groups.
    (h : replay2 s l ok = some kept) :
    -- The statement: the kept count is at least P (capped at the number of groups).
    min (proofOf l ok) l.stamps.length ≤ kept := by
  -- Unfold replay in the assumption.
  unfold replay2 at h
  -- Split on its first test, the stamp.
  split at h
  -- The stamp failed: replay refused, against "it opened".
  · cases h
  -- The stamp verified: split on the second test.
  · split at h
    -- O below P: replay refused, against "it opened".
    · cases h
    -- Otherwise; name the fact "O is not below P".
    · rename_i hge
      -- Replay kept the groups before O.
      simp only [Option.some.injEq] at h
      -- Replace `kept` by that number.
      subst h
      -- P is at most O: arithmetic.
      omega

/-- **Format 2 recovery yields a gap-free prefix that keeps every synced
commit.** For every state the writer reaches and every crash the device's
flush honoured, replay opens; recovery yields commits `0, ..., n - 1` for
some `n`; and it keeps every group a completed sync covered. -/
theorem recovers_prefix2 {w : Wal2} {ok : List Bool}
    -- `w` is reachable by the format 2 writer.
    (hreach : Steps2 init2 w)
    -- The crash kept the synced prefix.
    (hsurv : Survives2 w.log ok) :
    -- The statement: replay opens, keeping some number of groups;
    ∃ kept, replay2 true w.log ok = some kept ∧
      -- recovery yields commits 0, ..., (earlier + kept) - 1;
      recovered2 w kept = List.range (w.earlier.length + kept) ∧
      -- every synced group is kept; and no more groups are kept than exist.
      min w.log.synced w.log.stamps.length ≤ kept ∧ kept ≤ w.log.stamps.length := by
  -- A reachable log satisfies the invariant.
  obtain ⟨horder, hlog⟩ := reachable2_inv hreach
  -- The kept count is "the groups before O"; replay opens because the stamps are honest;
  refine ⟨min (firstBad ok) w.log.stamps.length,
    -- the commit range and the bound on synced groups are left to show.
    replay2_opens_of_honest (honest_of_inv hlog hsurv), ?_, ?_, Nat.min_le_right _ _⟩
  -- The earlier commits `0, ..., e - 1`, then `e, ..., e + kept - 1`.
  ·
    -- Unfold what recovery yields.
    unfold recovered2
    -- A range of e + kept numbers is the first e followed by the next kept; the first e are the
    -- earlier commits.
    rw [List.range_add, List.range'_eq_map_range, ← horder]
  -- O is at least the completed sync, since the synced prefix survived.
  ·
    -- That is the first part of "the crash kept the synced prefix".
    have := hsurv.1
    -- Arithmetic.
    omega

/-- **Residual damage is never silent.** Suppose a crash damaged bytes a
completed sync had covered, but the file still holds the synced prefix's
records. If replay opens and keeps fewer groups than were synced, it has
discarded a nonempty tail: records from O on are in the file, so the
discard is reported. -/
theorem residual_is_reported {s : Bool} {l : Log2} {ok : List Bool} {kept : Nat}
    -- The file holds at least the records the sync covered.
    (hlen : l.synced ≤ ok.length)
    -- Replay opened, keeping `kept` groups ...
    (h : replay2 s l ok = some kept)
    -- ... fewer than were synced.
    (hlost : kept < min l.synced l.stamps.length) :
    -- The statement: the first unusable record is inside the file, so a nonempty tail was dropped.
    firstBad ok < ok.length := by
  -- Unfold replay in the assumption.
  unfold replay2 at h
  -- Split on its first test, the stamp.
  split at h
  -- The stamp failed: replay refused, against "it opened".
  · cases h
  -- The stamp verified: split on the second test.
  · split at h
    -- O below P: replay refused, against "it opened".
    · cases h
    -- Otherwise replay kept the groups before O.
    · simp only [Option.some.injEq] at h
      -- Replace `kept` by that number.
      subst h
      -- Fewer than the synced groups were kept, and the file holds the synced records: so O is
      -- inside the file. Arithmetic.
      omega

/-- **A wrong key refuses.** With encryption at rest the stamp is sealed
under the key and was synced when the log was created; under the wrong
key its tag fails, and replay refuses rather than drop the log. -/
theorem wrong_key_refuses (l : Log2) (ok : List Bool) : replay2 false l ok = none := rfl

/-! ### The RED cases of the format 2 rule -/

/-- The defect: replay drops the tail at O even when O < P. -/
def replayDropBelowP (l : Log2) (ok : List Bool) : Option Nat :=
  -- Always open, keeping the groups before the first unusable record.
  some (min (firstBad ok) l.stamps.length)

/-- **`drop_below_p_loses_proven`.** Group 0 was synced; group 1 was
written after that sync completed, so it is stamped 1. A crash damages
group 0 (residual damage) and keeps group 1. P is 1 and O is 0: the
design refuses, while the defect opens with nothing, losing group 0 that
group 1's stamp proves synced. -/
theorem drop_below_p_loses_proven :
    -- The statement: P is 1; the engine refuses; the planted bug opens and keeps 0 groups.
    proofOf ⟨[0, 1], 1, false⟩ [false, true] = 1 ∧
    replay2 true ⟨[0, 1], 1, false⟩ [false, true] = none ∧
    replayDropBelowP ⟨[0, 1], 1, false⟩ [false, true] = some 0 := by
  -- All three are small computations: compute them.
  decide

/-- The defect: replay refuses every unusable record, a torn tail above P
included. -/
def replayRefuseAny (l : Log2) (ok : List Bool) : Option Nat :=
  -- Refuse when any record in the file is unusable; otherwise keep the groups before O.
  if firstBad ok < ok.length then none else some (min (firstBad ok) l.stamps.length)

/-- **`refuse_above_p_never_opens`.** Nothing synced yet; a crash keeps
group 0 and tears group 1. That crash honours every flush (`Survives2`),
and the design opens with group 0, but the defect refuses: a database
that never opens. -/
theorem refuse_above_p_never_opens :
    -- The statement: that crash honours every flush; the engine opens and keeps 1 group; the
    -- planted bug refuses.
    Survives2 ⟨[0, 0], 0, false⟩ [true, false] ∧
    replay2 true ⟨[0, 0], 0, false⟩ [true, false] = some 1 ∧
    replayRefuseAny ⟨[0, 0], 0, false⟩ [true, false] = none := by
  -- The two bounds of "honours every flush" and the two replays, each computed.
  refine ⟨⟨by decide, by decide⟩, by decide, by decide⟩

/-- What a crash may leave of one record, as format 1 tells them apart. -/
inductive Kind where
  /-- The record survived whole. -/
  | intact
  /-- The record is whole but its bytes are wrong. -/
  | garbage
  /-- The record's bytes read back as zeros: the length persisted, the
  data did not. -/
  | zero
  /-- The file ends inside the record: a torn write. -/
  | torn
  -- Lean may compare two kinds for equality.
  deriving DecidableEq

/-- Format 1's replay (E3): at the first record that is not intact, a
torn record, or zeros to the end of the file, end the log there; anything
else refuses. The result is how many records it keeps. -/
def replay1 : List Kind → Option Nat
  -- No record: keep nothing.
  | [] => some 0
  -- An intact record: one more than the rest keeps.
  | .intact :: rest => (replay1 rest).map (· + 1)
  -- A torn record: the log ends here.
  | .torn :: _ => some 0
  -- Any other record: end the log here if it and everything after it are zeros, else refuse.
  | k :: rest => if k = .zero ∧ rest.all (· == .zero) then some 0 else none

/-- **`format1_refuses_torn_tail`.** A crash keeps group 0 and leaves group
1 whole but wrong, as when its length persisted before its data. Format 1
refuses; format 2, where every unusable record is alike, opens with group
0. -/
theorem format1_refuses_torn_tail :
    -- The statement: format 1 refuses; format 2 opens and keeps 1 group.
    replay1 [.intact, .garbage] = none ∧
    replay2 true ⟨[0, 0], 0, false⟩ ([Kind.intact, .garbage].map (· == .intact)) = some 1 := by
  -- Both are small computations: compute them.
  decide

/-- **`unsealed_stamp_drops_log`.** Two groups, both synced. Under the
wrong key every record's tag fails, so no stamp can be read and P is 0.
If the stamp were not sealed under the key, it would verify, O would be 0
and replay would drop the whole log as a tail, losing both synced groups.
Sealed, it refuses (`wrong_key_refuses`). -/
theorem unsealed_stamp_drops_log :
    -- The statement: with a stamp that verifies under any key, replay opens keeping 0 groups; with
    -- a sealed stamp, it refuses.
    replay2 true ⟨[0, 1], 2, false⟩ [false, false] = some 0 ∧
    replay2 false ⟨[0, 1], 2, false⟩ [false, false] = none := by
  -- Both are small computations: compute them.
  decide

/-! ### Encryption at rest: the sealed stamp, durable before the log exists

The story. A database encrypted at rest seals each log's records and its
stamp with AES-256-GCM-SIV (`src/engine/wal_seal.rs`). Picture one log with
two records, a power cut, and a reopen. Two things can happen to the stamp:
it can be opened under the wrong key, and the power cut can tear it. The
engine seals the stamp under the key, so a wrong key fails it, and it makes
the stamp durable before the log has its name (`create_durably`: write a
staging file, sync it, rename it, sync the directory), so no power cut can
tear it. What this part shows:

1. `durable_stamp_whole`: with that creation, a power cut leaves the stamp
   whole.
2. `sealed_stamp_opens_after_honest_crash`: so the right key opens after
   any crash the device's flush honoured, with the same gap-free prefix as
   an unsealed log (`recovers_prefix2`).
3. `wrong_key_refuses_sealed`: the wrong key never opens, whatever the
   power cut left.
4. RED `unsynced_stamp_refuses_honest_crash`: written in place and left
   for the first record's sync, a stamp can be torn, and the right key is
   refused after an honest crash. (The other RED, a stamp nobody sealed, is
   `unsealed_stamp_drops_log` above.)
-/

/-- What a power cut left of a log's stamp: the bytes before the first
record of a file under `wal/`. -/
inductive StampState where
  /-- Every byte of the stamp reached the disk. -/
  | whole
  /-- Some byte of the stamp never reached the disk. -/
  | torn
  -- Two states can be compared, so `decide` can check small examples.
  deriving DecidableEq

/-- Whether a sealed stamp verifies, as `wal_seal::open_stamp` decides.
`keyOk` says whether the provider handed back the bytes the stamp was
sealed with. -/
def sealedStampOk (keyOk : Bool) : StampState → Bool
  -- A whole stamp's tag holds exactly when the key is right.
  | .whole => keyOk
  -- A torn stamp's tag fails whatever the key, like a wrong key.
  | .torn => false

/-- The stamp states a power cut can leave behind. `durableAtCreation` is
the engine's `create_durably`; without it (the defect) the stamp is
written in place, and can be torn only while nothing in the log is synced:
the first sync of a record makes the stamp before it durable too. -/
def StampSurvives (durableAtCreation : Bool) (l : Log2) (s : StampState) : Prop :=
  -- A whole stamp is always a possible outcome...
  s = .whole ∨
  -- ...a torn one only under the defect, before the log's first sync.
  (durableAtCreation = false ∧ l.synced = 0 ∧ s = .torn)

/-- **A stamp made durable at creation is whole after any power cut.** -/
theorem durable_stamp_whole {l : Log2} {s : StampState}
    -- What the power cut left, with the engine's creation.
    (h : StampSurvives true l s) :
    -- The statement: the stamp is whole.
    s = .whole := by
  -- Two cases: the stamp is whole, or the defect's torn case.
  rcases h with h | ⟨hdefect, _, _⟩
  -- Whole: that is the goal.
  · exact h
  -- Torn needs `true = false`, which is impossible, so this case is empty.
  · exact absurd hdefect (by decide)

/-- **The right key opens after an honest crash.** A reachable log, a crash
the device's flush honoured, a stamp made durable at creation: replay of
the sealed log opens, keeping a gap-free prefix that holds every synced
group, exactly as `recovers_prefix2` says of an unsealed log. -/
theorem sealed_stamp_opens_after_honest_crash {w : Wal2} {ok : List Bool} {s : StampState}
    -- `w` is reachable by the format 2 writer.
    (hreach : Steps2 init2 w)
    -- The crash kept the synced prefix; a failed tag counts as damage in `ok`.
    (hsurv : Survives2 w.log ok)
    -- What became of the stamp, with the engine's creation.
    (hstamp : StampSurvives true w.log s) :
    -- The statement: replay under the right key opens, keeping some number of groups;
    ∃ kept, replay2 (sealedStampOk true s) w.log ok = some kept ∧
      -- recovery yields commits 0, ..., (earlier + kept) - 1;
      recovered2 w kept = List.range (w.earlier.length + kept) ∧
      -- every synced group is kept; and no more groups are kept than exist.
      min w.log.synced w.log.stamps.length ≤ kept ∧ kept ≤ w.log.stamps.length := by
  -- The stamp is whole...
  rw [durable_stamp_whole hstamp]
  -- ...so under the right key it verifies, and replay is the unsealed one,
  -- which `recovers_prefix2` already shows opens on a gap-free prefix.
  exact recovers_prefix2 hreach hsurv

/-- **The wrong key never opens a sealed log.** Whatever the power cut left
of the stamp, its tag fails under the wrong key, and replay refuses. -/
theorem wrong_key_refuses_sealed (l : Log2) (ok : List Bool) (s : StampState) :
    -- The statement: under the wrong key, replay refuses.
    replay2 (sealedStampOk false s) l ok = none := by
  -- Look at both things the stamp can be.
  cases s
  -- Whole under the wrong key: the stamp check is `false`, and replay
  -- refuses at its first line.
  · rfl
  -- Torn: the stamp check is `false` too, and replay refuses the same way.
  · rfl

/-- **RED `unsynced_stamp_refuses_honest_crash`.** The defect writes the
sealed stamp in place. A fresh log, nothing synced in it, and the power
goes: the stamp comes back torn, which `StampSurvives false` allows, and the
crash honoured every flush (`Survives2`). Reopened with the right key, the
torn stamp fails and replay refuses: a database the right key cannot open
after a crash it should survive. With the engine's creation that stamp
state cannot happen (`durable_stamp_whole`). -/
theorem unsynced_stamp_refuses_honest_crash :
    -- The defect allows a torn stamp on a fresh, unsynced log...
    StampSurvives false ⟨[], 0, false⟩ .torn ∧
    -- ...the crash kept everything a sync covered (nothing)...
    Survives2 ⟨[], 0, false⟩ [] ∧
    -- ...and the right key is refused.
    replay2 (sealedStampOk true .torn) ⟨[], 0, false⟩ [] = none := by
  -- Prove the three parts one at a time.
  refine ⟨?_, ?_, ?_⟩
  -- The torn case of `StampSurvives`: defect, nothing synced, torn.
  · unfold StampSurvives
    -- Take the second alternative, all three of whose equalities hold.
    exact Or.inr ⟨rfl, rfl, rfl⟩
  -- `Survives2`: nothing was synced, and the empty file holds no record.
  · unfold Survives2
    -- `0 ≤ 0` and `0 ≤ total`, both by arithmetic Lean can evaluate.
    exact ⟨Nat.le_refl _, Nat.zero_le _⟩
  -- Replay with a stamp check of `false` refuses at its first line.
  · rfl

-- The end of this file's names.
end Regolith.WalRecovery
