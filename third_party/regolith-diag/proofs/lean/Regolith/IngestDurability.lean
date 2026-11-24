/-!
# IngestDurability: an ingest that survives keeps every commit before it (D48)

This file backs the TLA+ model `proofs/tla/IngestDurability.tla`, invariant
`GapFreePrefix` (configurations `MC_IngestDurability_Green` and
`MC_IngestDurability_Red_NoLogSync`). The model checks three commits racing
one ingest; this file proves the law for any number of commits.

## The story, with a tiny example

Under `Eventual` durability a commit's record goes into the write-ahead log
and nobody waits for it to reach the disk. A power cut must still leave a
gap-free prefix of commit order: if commit 5 survives, commits 0 to 4 do too.

An ingest is a commit in that order too: its install is an ordered unit of
the commit ring (`src/engine/commit/ordered.rs`), which runs with every
earlier commit written and no later one decided, and takes its place there
like any commit. But it is made durable by its own channel, the synced
manifest batch that adds its table, not by the log.

Example: commit `c0` writes `a = 1` into the log, unsynced. Then an ingest
takes its place after `c0` and syncs its manifest batch. If the power goes
out now, the ingest survives (its batch is on the disk) while `c0` is lost
(its log record never was). Reads would show the ingest but not the commit
ordered before it: a gap.

The fix (`Ordered::sync_log`, called by `RegolithEngine::install_ordered` in
`src/engine/ingest.rs`): in its ordered unit, before its manifest batch, the
ingest syncs the log, so every commit ordered before it is on the disk first.

## What is proved, in plain words

1. `step_inv` and `reachable_inv`: the fixed protocol keeps its invariant:
   the durable commits are a prefix, and once the ingest is installed, every
   commit before its place is durable.
2. `surviving_ingest_keeps_prefix`: so after a power cut at any moment, if
   the ingest survives, every commit ordered before it survives, and what
   survives is a gap-free prefix of commit order.
3. `no_log_sync_loses_commit`: without the log sync, one commit, the
   ingest, and a power cut leave the ingest without the commit before it.
   This is the RED configuration as a concrete counterexample.
-/

-- Everything below lives in this namespace, so its names do not clash.
namespace Regolith.IngestDurability

/-- What the protocol works on. Commits are numbered `0, 1, 2, ...` in commit
order: slots are decided in ring order, and none while a unit runs. -/
structure St where
  /-- How many commits have drawn their place; all are in the log. -/
  commits : Nat
  /-- How many leading commits the log has synced: commits `0 .. synced - 1`
  survive a power cut now. -/
  synced : Nat
  /-- The ingest's ordered unit is running, between its log sync and its
  install: it has the pipeline to itself. -/
  holding : Bool
  /-- `some p` once the ingest is installed, with `p` the number of commits
  ordered before it; `none` before. Its synced manifest batch makes it
  survive from then on. -/
  ingestAt : Option Nat

/-- The start: nothing committed, synced, held or installed. -/
def init : St := ⟨0, 0, false, none⟩

/-- One step. `sync` says whether the ingest syncs the log before its batch:
`true` is the fix, `false` the RED `NoLogSync`. -/
inductive Step (sync : Bool) : St → St → Prop
  /-- A commit is decided, which it is not while the ingest's unit runs, and
  its record is written to the log, unsynced. -/
  | commit (s : St) (h : s.holding = false) :
      -- After the step: one more commit; nothing else changes.
      Step sync s { s with commits := s.commits + 1 }
  /-- The log syncs (a rotation, or a sync round for a commit that asked for
  `Immediate`): every commit in it becomes durable. -/
  | syncLog (s : St) (h : s.holding = false) :
      -- After the step: the synced count catches up with the commits.
      Step sync s { s with synced := s.commits }
  /-- The ingest's unit begins and, in the fix, syncs the log. -/
  | ingestSync (s : St) (h : s.holding = false) (hn : s.ingestAt = none) :
      -- After the step: the ingest's unit is running,
      Step sync s { s with holding := true,
                           synced := if sync then s.commits else s.synced } -- and the fix syncs every commit so far
  /-- Still in its unit, the ingest takes its place after every commit so
  far, its synced manifest batch installs it, and the unit ends. -/
  | ingestInstall (s : St) (h : s.holding = true) :
      -- After the step: the unit is over, the ingest installed after every commit so far.
      Step sync s { s with holding := false, ingestAt := some s.commits }

/-- Any number of steps, one after another. -/
inductive Steps (sync : Bool) : St → St → Prop
  /-- No step at all. -/
  | refl (s : St) : Steps sync s s
  /-- Some steps, then one more. -/
  | tail {s t u : St} : Steps sync s t → Step sync t u → Steps sync s u

/-- The fixed protocol's invariant. -/
structure Inv (s : St) : Prop where
  /-- The synced commits are among those made. -/
  synced_le : s.synced ≤ s.commits
  /-- While the ingest's unit runs, the log is synced through every commit,
  and the ingest is not installed yet. -/
  holding_synced : s.holding = true → s.synced = s.commits ∧ s.ingestAt = none
  /-- Once installed, every commit ordered before the ingest is synced. -/
  installed_synced : ∀ p, s.ingestAt = some p → p ≤ s.synced

/-- The start satisfies the invariant. -/
theorem init_inv : Inv init :=
  -- Nothing is synced, nothing is held, nothing is installed.
  ⟨Nat.le_refl 0, fun h => absurd h (by decide), fun _ h => absurd h (by simp [init])⟩

/-- **Every step of the fixed protocol keeps the invariant.** -/
theorem step_inv {s t : St}
    -- The invariant holds before the step,
    (hinv : Inv s)
    -- and the step is one of the fixed protocol's.
    (hstep : Step true s t) : Inv t := by
  -- Unpack the three parts of the invariant.
  obtain ⟨hle, hhold, hinst⟩ := hinv
  -- Look at which step it was.
  cases hstep with
  -- Case: a commit.
  | commit h =>
    -- A commit adds to `commits` only: synced stays below, nothing is held.
    refine ⟨by simp only; omega, fun h' => absurd h' (by simp [h]), ?_⟩
    -- The ingest's place and the synced count are unchanged.
    exact fun p hp => hinst p hp
  -- Case: a log sync.
  | syncLog h =>
    -- Syncing makes `synced = commits`, which is at least the old `synced`.
    refine ⟨by simp only; omega, fun h' => absurd h' (by simp [h]), ?_⟩
    -- An installed ingest's place was at most the old count, hence the new.
    intro p hp
    -- The old bound,
    have := hinst p hp
    -- and the old count is at most the new one.
    simp only; omega
  -- Case: the ingest's unit begins and syncs.
  | ingestSync h hn =>
    -- The fix syncs everything and the unit runs; nothing is installed.
    refine ⟨by simp, fun _ => ⟨by simp, hn⟩, ?_⟩
    -- Not installed, so the last part holds vacuously.
    intro p hp
    -- `ingestAt` is still `none`, not `some p`.
    simp only [hn] at hp
    -- `none = some p` is false.
    exact absurd hp (by simp)
  -- Case: the ingest installs.
  | ingestInstall h =>
    -- While holding, the log was synced through every commit.
    obtain ⟨hsyn, _⟩ := hhold h
    -- Letting go keeps the counts; nothing is held now.
    refine ⟨hle, fun h' => absurd h' (by simp), ?_⟩
    -- The ingest's place is the commit count, which equals `synced`.
    intro p hp
    -- `some commits = some p`, so `p = commits`.
    simp only [Option.some.injEq] at hp
    -- So `p = commits = synced`.
    simp only; omega

/-- **Every reachable state of the fixed protocol satisfies the invariant.** -/
theorem reachable_inv {s : St}
    -- `s` is reached from the start by the fixed protocol.
    (h : Steps true init s) : Inv s := by
  -- Walk the steps from the start.
  induction h with
  -- No step: the start satisfies the invariant
  | refl => exact init_inv
  -- One more step: the invariant carries over it
  | tail _ hstep ih => exact step_inv ih hstep

/-- What a power cut leaves of commit `c`: it survives iff it is synced. -/
def CommitSurvives (s : St) (c : Nat) : Prop := c < s.synced

/-- **An ingest that survives a power cut keeps every commit ordered before
it.** At any reachable state, if the ingest is installed (so it survives),
every one of the `p` commits before its place survives too. And what
survives of the commits is a gap-free prefix: commit `c` survives exactly
when every commit before it does, and it is among the first `synced`. -/
theorem surviving_ingest_keeps_prefix {s : St}
    -- `s` is reached by the fixed protocol,
    (h : Steps true init s) :
    -- an installed ingest keeps every commit before its place,
    (∀ p, s.ingestAt = some p → ∀ c, c < p → CommitSurvives s c) ∧
    -- and a surviving commit's predecessors all survive.
    (∀ c, CommitSurvives s c → ∀ c', c' < c → CommitSurvives s c') := by
  -- The invariant holds here.
  have hinv := reachable_inv h
  -- Two claims.
  refine ⟨?_, ?_⟩
  -- Take the ingest's place `p` and a commit `c` before it.
  ·
    -- Name them.
    intro p hp c hc
    -- The invariant bounds `p` by the synced count,
    have := hinv.installed_synced p hp
    -- so `c < p ≤ synced`.
    unfold CommitSurvives; omega
  -- A prefix: `c' < c < synced`.
  ·
    -- Take a surviving commit c and an earlier commit c'.
    intro c hc c' hc'
    -- Unfold and compare.
    unfold CommitSurvives at *; omega

/-- **RED `NoLogSync`.** Without the log sync, one commit, the ingest's two
steps, and a power cut leave the ingest installed (surviving) while commit 0,
ordered before it, is not synced and is lost. -/
theorem no_log_sync_loses_commit :
    -- The statement: some run without the log sync ends with the ingest installed after one commit,
    -- and commit 0 lost.
    ∃ s, Steps false init s ∧ s.ingestAt = some 1 ∧ ¬ CommitSurvives s 0 := by
  -- The state after: one commit, nothing synced, the ingest installed after it.
  refine ⟨⟨1, 0, false, some 1⟩, ?_, rfl, by unfold CommitSurvives; decide⟩
  -- Commit 0, then the ingest's (skipped) sync, then its install.
  exact .tail (.tail (.tail (.refl _) (.commit _ rfl))
    (.ingestSync _ rfl rfl)) (.ingestInstall _ rfl) -- the ingest skips its sync, then installs

-- End of the namespace opened at the top.
end Regolith.IngestDurability
