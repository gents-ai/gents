/-!
# ManifestRecovery: a torn manifest end (E29) and the logs a flush retires (E30)

This file backs the TLA+ model `proofs/tla/ManifestRecovery.tla`
(configurations `MC_ManifestRecovery_*`): `crash_never_proves` is its
`RecoveryOpens`, `replay_is_prefix` and `replay_keeps_synced` are the
manifest half of `AckedSurvive`, and `replay_never_above_newer_table` with
`memtable_newest_is_newest` is `ReadsNewest`. The model checks a few runs
step by step; this file proves the same laws for every size.

## The story, with a tiny example

The MANIFEST is regolith's list of the table files that make up the
database: a run of batches written one after another. A batch that adds a
table is synced (the writer waits until it is on the disk) before the next
batch is written. A batch that only reserves a file number is not.

Example: a new database writes `a = 1`, then flushes it. The flush appends
"reserve 2", "reserve 3", then "table 3 is in", and syncs. If the power goes
out during that sync, the last three batches can come back as garbage. That
is a crash's doing, and the write-ahead log still holds `a = 1`.

E29: the open used to refuse such a database. The fix refuses only when a
whole batch that needed a sync sits after the damage with more bytes after
it: those bytes were written after that sync finished, and the sync made
the damaged spot durable, so a crash cannot have damaged it.

E30: after a flush the engine deletes the log the table covers. If the
delete failed, the next open replayed that log into memory, which is read
before any table, so an old value came back over a newer table. The fix
records `min_wal_id` in the table's own batch, and recovery skips every log
below it.

## What is proved, in plain words

1. `crash_never_proves`: when the writer syncs every batch that needs it
   before writing on, no damage a crash leaves is ever "proved synced", so
   the open accepts every crash state.
2. `replay_is_prefix` and `replay_keeps_synced`: what replay keeps is exactly
   the batches the writer wrote, in order, and at least every synced one.
3. `replay_never_above_newer_table`: with `min_wal_id` in the table's batch,
   every version recovery puts in memory is newer than every version any
   table holds of the same key; `memtable_newest_is_newest` turns that into
   "reading memory first returns the newest version".
4. `no_sync_before_next_refuses_a_crash` and `stale_without_min_wal`: the two
   RED cases as concrete counterexamples.
5. `crash_never_proves_opened`: on a sealed manifest the judge opens each
   batch before it reads whether the writer synced it, and then accepts every
   crash state; `judging_ciphertext_refuses_a_crash` is the RED where it reads
   the sealed bytes as plain records (`MC_ManifestRecovery_Red_JudgeCiphertext`).
6. `oldest_first_keeps_every_log`: with several memtables waiting and flushes
   on several threads, every flush takes the oldest, so every version of every
   log comes back from a table or replayed; `newest_first_loses_a_log` is the
   RED where a newer memtable is flushed first
   (`MC_ManifestRecovery_Red_NewestFirst`).

The code each definition mirrors is named on it. The engine side lives in
`src/engine/manifest/tail.rs` (the judgment), `src/engine/manifest.rs`
(`VersionSet::apply`, `judge_end`) and `src/engine/log_retirement.rs`.
-/

-- Everything below lives in this namespace, so its names do not clash.
namespace Regolith.ManifestRecovery

/-! ## Part 1 (E29): the manifest's damaged end -/

/-- One batch of the manifest. Only one thing about it matters here: whether
the writer syncs it before writing the next one (`ManifestRecord::
requires_sync` in `src/engine/manifest.rs`: true for a batch that adds or
removes a table, false for one that only reserves a number). -/
structure Batch where
  /-- `true` when `VersionSet::apply` waits for this batch to reach the disk. -/
  needsSync : Bool

/-- The manifest as the writer left it at the moment of the crash. -/
structure Manifest where
  /-- How many batches the file holds. -/
  len : Nat
  /-- The batch at each position `0, 1, ..., len - 1`. -/
  batch : Nat → Batch
  /-- How many leading batches a finished sync has put on the disk. -/
  synced : Nat

/-- The writer's rule (`VersionSet::apply` syncs before it returns, and holds
the writer until then): a batch that needs a sync and has another batch
after it was synced before that next batch was written, so it lies in the
synced prefix. -/
def SyncedBeforeNext (m : Manifest) : Prop :=
  -- The synced prefix lies inside the file,
  m.synced ≤ m.len ∧
  -- and every sync-needing batch with a batch after it is inside that prefix.
  ∀ j, j + 1 < m.len → (m.batch j).needsSync = true → j < m.synced

/-- What the open reads back: at each position either the batch the writer
wrote (`some`) or something that does not read back whole (`none`). -/
abbrev Image := Nat → Option Batch

/-- `img` is what a power cut can leave of `m`: every synced batch is intact,
and every other batch is either intact or damaged, in any pattern (a cut
short file, zeros, garbage, or sectors written out of order). -/
def CrashImage (m : Manifest) (img : Image) : Prop :=
  -- For every position inside the file:
  ∀ j, j < m.len →
    -- A synced batch reads back as written,
    (j < m.synced → img j = some (m.batch j)) ∧
    -- and any batch reads back as written or not at all.
    (img j = some (m.batch j) ∨ img j = none)

/-- `o` is where replay stops (`tail::batch_at` fails there first): the first
position that does not read back whole. -/
def FirstTorn (len : Nat) (img : Image) (o : Nat) : Prop :=
  -- It is inside the file,
  o < len ∧
  -- it does not read back whole,
  img o = none ∧
  -- and everything before it does.
  ∀ i, i < o → img i ≠ none

/-- `tail::proof_past`: after the stop at `o`, a whole batch that needed a
sync, at a position `j` that is not the last, so bytes follow it. -/
def Proof (len : Nat) (img : Image) (o : Nat) : Prop :=
  -- Some position j past o, not the last, holds a whole sync-needing batch
  ∃ j b, o < j ∧ j + 1 < len ∧ img j = some b ∧ b.needsSync = true

/-- In a crash image, replay never stops inside the synced prefix: every
synced batch reads back whole. -/
theorem first_torn_ge_synced {m : Manifest} {img : Image} {o : Nat}
    -- `img` is something a crash left of `m`,
    (hc : CrashImage m img)
    -- and replay stops at `o`.
    (hf : FirstTorn m.len img o) :
    -- Claim: the stop is at or past the end of the synced prefix
    m.synced ≤ o := by
  -- Unpack the stop: `o` is inside the file and reads back as nothing.
  obtain ⟨hlen, hnone, _⟩ := hf
  -- Either `o` is at or past the synced prefix, or it is inside it.
  by_cases hlt : o < m.synced
  -- Inside the prefix, the crash rule says `o` reads back as written...
  ·
    -- The crash rule, for a position inside the synced prefix.
    have hsome := (hc o hlen).1 hlt
    -- ...which is not "nothing", so this case cannot happen.
    rw [hnone] at hsome
    -- `none = some _` is false on its face.
    exact absurd hsome (by simp)
  -- Outside it, the goal is the negation of `hlt`, which `omega` reads off.
  ·
    -- Arithmetic.
    omega

/-- **The open accepts every crash state.** When the writer keeps its rule,
no damage a crash leaves is proved synced, so `tail::judge` drops it rather
than refusing. This rules out the old refusal of a first flush cut. -/
theorem crash_never_proves {m : Manifest} {img : Image} {o : Nat}
    -- The writer synced every batch that needed it before writing on,
    (hw : SyncedBeforeNext m)
    -- `img` is something a crash left,
    (hc : CrashImage m img)
    -- and replay stopped at `o`.
    (hf : FirstTorn m.len img o) :
    -- Claim: no proof follows the stop
    ¬ Proof m.len img o := by
  -- Suppose a proof exists: a whole sync-needing batch `b` at `j` past `o`.
  intro ⟨j, b, hoj, hjlen, himg, hsync⟩
  -- Replay stopped at or past the synced prefix.
  have hge := first_torn_ge_synced hc hf
  -- Position `j` is inside the file, since a batch follows it.
  have hj : j < m.len := by omega
  -- The crash rule says `j` reads back as written or as nothing.
  rcases (hc j hj).2 with hw' | hn
  -- It reads back as written, so `b` is the batch the writer wrote there.
  ·
    -- Put b for what the image reads at j.
    rw [himg] at hw'
    -- Peel off `some` on both sides: `b` is `m.batch j`.
    injection hw' with hb
    -- So the batch the writer wrote at `j` needed a sync.
    rw [hb] at hsync
    -- The writer's rule puts `j`, which has a batch after it, in the synced prefix.
    have := hw.2 j hjlen hsync
    -- But `j` lies past `o`, which lies at or past that prefix: impossible.
    omega
  -- It reads back as nothing, yet `himg` says it reads back as `b`.
  ·
    -- Put b for what the image reads at j: "some b" would be "none".
    rw [himg] at hn
    -- `some b = none` is false on its face.
    exact absurd hn (by simp)

/-- **Replay keeps a prefix of what was written.** Every position before the
stop reads back as exactly the batch the writer wrote there. -/
theorem replay_is_prefix {m : Manifest} {img : Image} {o : Nat}
    -- `img` is something a crash left,
    (hc : CrashImage m img)
    -- and replay stopped at `o`.
    (hf : FirstTorn m.len img o) :
    -- Claim: each kept position reads back as the batch written there
    ∀ i, i < o → img i = some (m.batch i) := by
  -- Take any position `i` before the stop.
  intro i hi
  -- It is inside the file, as `o` is.
  have hil : i < m.len := by have := hf.1; omega
  -- The crash rule: it reads back as written or as nothing.
  rcases (hc i hil).2 with h | h
  -- As written: that is the goal.
  ·
    -- Exactly what is asked.
    exact h
  -- As nothing: but everything before the stop reads back whole.
  ·
    -- Before the stop nothing reads back as nothing.
    exact absurd h (hf.2.2 i hi)

/-- **Replay keeps every synced batch.** The prefix it keeps reaches at least
the end of the synced prefix, so no batch a sync made durable is dropped:
every table such a batch adds, and its `min_wal_id`, survive. -/
theorem replay_keeps_synced {m : Manifest} {img : Image} {o : Nat}
    -- `img` is something a crash left,
    (hc : CrashImage m img)
    -- and replay stopped at `o`.
    (hf : FirstTorn m.len img o) :
    -- Claim: each synced position is kept, and reads back as written
    ∀ i, i < m.synced → i < o ∧ img i = some (m.batch i) := by
  -- Take any synced position.
  intro i hi
  -- It lies before the stop, since the stop is at or past the synced prefix.
  have hio : i < o := by have := first_torn_ge_synced hc hf; omega
  -- So it is before the stop, and reads back as written.
  exact ⟨hio, replay_is_prefix hc hf i hio⟩

/-- **RED `NoSyncBeforeNext`.** A writer that writes on before a batch's sync
finishes lets a crash produce a "proof": batch 0 needs a sync and is torn,
batch 1 needs a sync, is whole and has batch 2 after it. The open would
refuse a state a crash produced. -/
theorem no_sync_before_next_refuses_a_crash :
    -- Three batches; the first two need a sync; nothing was synced yet.
    let m : Manifest := ⟨3, fun j => ⟨j < 2⟩, 0⟩
    -- The crash tears batch 0 and keeps batches 1 and 2.
    let img : Image := fun j => if j = 0 then none else some (m.batch j)
    -- The writer broke its rule, the image is a crash image, replay stops
    -- at 0, and a proof follows.
    ¬ SyncedBeforeNext m ∧ CrashImage m img ∧ FirstTorn m.len img 0 ∧
      Proof m.len img 0 := by -- the four facts together
  -- Unfold the two `let`s into the goal.
  intro m img
  -- Four claims, proved one by one.
  refine ⟨?_, ?_, ?_, ?_⟩
  -- The rule would put batch 0 (sync-needing, batch 1 after it) below `synced = 0`, which no number
  -- is.
  ·
    -- Suppose the writer's rule holds; take its second half.
    intro ⟨_, h⟩
    -- Apply the rule to batch 0: it claims `0 < 0`.
    have := h 0 (by decide) (by decide)
    -- `0 < 0` is false.
    exact absurd this (by decide)
  -- Every position is intact or torn, and nothing is synced.
  ·
    -- Take a position j inside the file.
    intro j _
    -- Nothing is synced, so the first half holds vacuously.
    refine ⟨fun h => absurd h (Nat.not_lt_zero _), ?_⟩
    -- Position 0 is torn, every other one intact.
    by_cases h0 : j = 0
    -- Torn: the image reads nothing there.
    ·
      -- The second alternative: torn.
      right
      -- Unfold the image at 0.
      simp [img, h0]
    -- Intact: the image reads the batch written there.
    ·
      -- The first alternative: intact.
      left
      -- Unfold the image away from 0.
      simp [img, h0]
  -- Replay stops at 0: inside the file, torn, nothing before it.
  ·
    -- 0 is inside the file; the image reads nothing at 0; no position is below 0.
    refine ⟨by decide, by simp [img], fun i hi => absurd hi (Nat.not_lt_zero _)⟩
  -- Batch 1 is whole, needs a sync, and batch 2 follows it.
  ·
    -- The witness: position 1, a sync-needing batch, past 0, not the last, and whole in the image.
    exact ⟨1, ⟨true⟩, by decide, by decide, by simp [img, m], rfl⟩

/-! ## Part 1b: the judge on a sealed manifest (encryption meets E29) -/

/-- `tail::proof_past` with the judge's reading made explicit: `seen b` is
what the judge reads off a whole batch `b` for "did the writer sync you
before writing on?". On a plain manifest it reads the batch's own flag. On
a sealed one the batch is noise until `sealed::open_batch` opens it. -/
def ProofBy (seen : Batch → Bool) (len : Nat) (img : Image) (o : Nat) : Prop :=
  -- A whole batch past o, not the last, read as synced
  ∃ j b, o < j ∧ j + 1 < len ∧ img j = some b ∧ seen b = true

/-- **The judge that opens each batch first accepts every crash state, sealed
or not.** `tail::needs_sync` opens a sealed batch before it decodes its
records, so what it reads is the batch's true flag, `Batch.needsSync`, and
the judgment is exactly the plain one: `crash_never_proves` applies. -/
theorem crash_never_proves_opened {m : Manifest} {img : Image} {o : Nat}
    -- The writer synced every batch that needed it before writing on,
    (hw : SyncedBeforeNext m)
    -- `img` is something a crash left,
    (hc : CrashImage m img)
    -- and replay stopped at `o`.
    (hf : FirstTorn m.len img o) :
    -- Claim: reading true flags, no proof follows the stop
    ¬ ProofBy Batch.needsSync m.len img o :=
  -- `ProofBy Batch.needsSync` unfolds to `Proof`, which the plain theorem rules out.
  crash_never_proves hw hc hf

/-- **RED `JudgeCiphertext`.** A judge that reads sealed bytes as plain
records cannot decode them and counts every whole batch as synced
(`seen := fun _ => true`). Three reservations, none needing a sync, nothing
synced yet; a crash tears the first and keeps the other two. The writer kept
its rule, the image is a crash image, replay stops at 0, and yet the judge
finds a "proof" and would refuse a database a crash left. -/
theorem judging_ciphertext_refuses_a_crash :
    -- Three batches; none needs a sync; nothing was synced yet.
    let m : Manifest := ⟨3, fun _ => ⟨false⟩, 0⟩
    -- The crash tears batch 0 and keeps batches 1 and 2.
    let img : Image := fun j => if j = 0 then none else some (m.batch j)
    -- The writer kept its rule, the image is a crash image, replay stops at
    -- 0, and the judge that reads every batch as synced finds a proof.
    SyncedBeforeNext m ∧ CrashImage m img ∧ FirstTorn m.len img 0 ∧
      ProofBy (fun _ => true) m.len img 0 := by -- the four facts together
  -- Unfold the two `let`s into the goal.
  intro m img
  -- Four claims, proved one by one.
  refine ⟨?_, ?_, ?_, ?_⟩
  -- `synced = 0` lies inside the file, and no batch needs a sync, so the rule has nothing to say
  -- about any of them.
  ·
    -- 0 is at most the length; for the rest, take a batch j said to need a sync.
    refine ⟨by decide, fun j _ h => ?_⟩
    -- `h` claims batch `j` needs a sync, but every batch here says `false`.
    simp [m] at h
  -- Every position is intact or torn, and nothing is synced.
  ·
    -- Take a position j inside the file.
    intro j _
    -- Nothing is synced, so the first half holds vacuously.
    refine ⟨fun h => absurd h (Nat.not_lt_zero _), ?_⟩
    -- Position 0 is torn, every other one intact.
    by_cases h0 : j = 0
    -- Torn: the image reads nothing there.
    ·
      -- The second alternative: torn.
      right
      -- Unfold the image at 0.
      simp [img, h0]
    -- Intact: the image reads the batch written there.
    ·
      -- The first alternative: intact.
      left
      -- Unfold the image away from 0.
      simp [img, h0]
  -- Replay stops at 0: inside the file, torn, nothing before it.
  ·
    -- 0 is inside the file; the image reads nothing at 0; no position is below 0.
    refine ⟨by decide, by simp [img], fun i hi => absurd hi (Nat.not_lt_zero _)⟩
  -- Batch 1 is whole and batch 2 follows it; the judge reads it as synced.
  ·
    -- The witness: position 1, past 0, not the last, whole in the image, and read as synced by the
    -- judge.
    exact ⟨1, ⟨false⟩, by decide, by decide, by simp [img, m], rfl⟩

/-! ## Part 2 (E30): no replayed version above a newer table -/

/-- A version of a key: the key and the sequence number that wrote it. -/
structure Version where
  /-- The user key. -/
  key : Nat
  /-- The sequence number; a larger one is newer. -/
  seq : Nat

/-- Logs, numbered oldest first: `logs i v` says log `i` holds version `v`. -/
abbrev Logs := Nat → Version → Prop

/-- Logs are sealed in order (`seal_active`): every version in an older log
was written before every version in a newer one. -/
def SealOrder (logs : Logs) : Prop :=
  -- An older log only holds older versions than a newer log
  ∀ i j v w, i < j → logs i v → logs j w → v.seq < w.seq

/-- The ingest rule (`flush_memtables_holding` in `src/engine/ingest.rs`):
an ingest flushes every memtable holding a key of its file first, so any
version of that key in a log still unflushed at the crash (number `f` or
above) was written after the ingest. -/
def IngestBeside (logs : Logs) (f : Nat) (ingested : Version → Prop) : Prop :=
  -- An unflushed log only holds versions of an ingested key newer than it
  ∀ t r j, ingested t → f ≤ j → logs j r → r.key = t.key → t.seq < r.seq

/-- What recovery puts in the memtable (`should_replay_wal`): every version
of every log numbered `minWal` or above. A log that was flushed but whose
delete failed is still on disk; this is what decides whether it is read. -/
def Replayed (logs : Logs) (minWal : Nat) (v : Version) : Prop :=
  -- Some log at or above minWal holds it
  ∃ j, minWal ≤ j ∧ logs j v

/-- What the tables hold: every version of the `f` flushed logs, and every
ingested version. -/
def InTables (logs : Logs) (f : Nat) (ingested : Version → Prop) (v : Version) : Prop :=
  -- Some flushed log holds it, or an ingest wrote it
  (∃ j, j < f ∧ logs j v) ∨ ingested v

/-- **No replayed version sits above a newer table.** When the flush writes
`min_wal_id = f` in the table's own batch (logs `0 .. f - 1` flushed), every
version recovery puts in memory is newer than every version of the same key
any table holds. This rules out the stale read of E30. -/
theorem replay_never_above_newer_table {logs : Logs} {f minWal : Nat}
    -- What the ingests wrote
    {ingested : Version → Prop}
    -- Logs were sealed in order,
    (hs : SealOrder logs)
    -- ingests never land under a memtable holding their key,
    (hi : IngestBeside logs f ingested)
    -- and the table's batch recorded every flushed log as retired.
    (hm : minWal = f) :
    -- The statement: for a replayed version r and a table version t of the same key, t is older
    -- than r.
    ∀ r t, Replayed logs minWal r → InTables logs f ingested t → r.key = t.key →
      t.seq < r.seq := by -- t is older than r
  -- Take a replayed version `r` and a table version `t` of the same key.
  intro r t ⟨j, hj, hr⟩ ht hkey
  -- The table version came from a flushed log or from an ingest.
  rcases ht with ⟨i, hi', ht⟩ | hing
  -- From flushed log `i`: it is older than replayed log `j`, as `i < f ≤ j`.
  ·
    -- The seal order, for log i below log j.
    exact hs i j t r (by omega) ht hr
  -- From an ingest: the replayed log is unflushed, so the ingest rule says `r` came after it.
  ·
    -- The ingest rule, for the unflushed log j.
    exact hi t r j hing (by omega) hr hkey

/-- **Reading memory first returns the newest version.** If `r` is the newest
version of its key in memory, nothing in memory or in any table is newer. -/
theorem memtable_newest_is_newest {logs : Logs} {f minWal : Nat}
    -- What the ingests wrote
    {ingested : Version → Prop}
    -- The three conditions of `replay_never_above_newer_table`,
    (hs : SealOrder logs) (hi : IngestBeside logs f ingested) (hm : minWal = f)
    -- and `r` is a replayed version,
    (r : Version) (hr : Replayed logs minWal r)
    -- newest of its key among the replayed ones.
    (hnew : ∀ r', Replayed logs minWal r' → r'.key = r.key → r'.seq ≤ r.seq) :
    -- The statement: any version of the key, in memory or in a table, is no newer than r.
    ∀ v, (Replayed logs minWal v ∨ InTables logs f ingested v) → v.key = r.key →
      v.seq ≤ r.seq := by -- is no newer than r
  -- Take any version `v` of the same key, in memory or in a table.
  intro v hv hkey
  -- Look at where it lives.
  rcases hv with hv | hv
  -- In memory: `r` is the newest there.
  ·
    -- The assumption on r, applied to v.
    exact hnew v hv hkey
  -- In a table: every replayed version, `r` included, is newer.
  ·
    -- The theorem above: the table version v is older than the replayed r.
    have := replay_never_above_newer_table hs hi hm r v hr hv hkey.symm
    -- Strictly newer is in particular at least as new.
    omega

/-- **RED `NoMinWalId`.** Without `min_wal_id`, recovery reads every log on
disk (`minWal = 0`). Log 0 holds key 0 at sequence 1 and was flushed (`f =
1`), but its delete failed; an ingest then wrote key 0 at sequence 2. The
memtable's answer, sequence 1, sits above the table's newer sequence 2. -/
theorem stale_without_min_wal :
    -- Log 0 holds `(0, 1)`; no other log holds anything.
    let logs : Logs := fun i v => i = 0 ∧ v = ⟨0, 1⟩
    -- The ingest wrote `(0, 2)`.
    let ingested : Version → Prop := fun v => v = ⟨0, 2⟩
    -- Logs are sealed in order and the ingest rule holds, yet a replayed
    -- version is not newer than a table's version of its key.
    SealOrder logs ∧ IngestBeside logs 1 ingested ∧
      ∃ r t, Replayed logs 0 r ∧ InTables logs 1 ingested t ∧ r.key = t.key ∧ -- yet there is a replayed r and a table t of the same key
        ¬ t.seq < r.seq := by -- with t not older than r
  -- Unfold the two `let`s into the goal.
  intro logs ingested
  -- Three claims, proved one by one.
  refine ⟨?_, ?_, ?_⟩
  -- Only log 0 holds anything, so no two different logs both hold one.
  ·
    -- Take logs i below j holding versions v and w.
    intro i j v w hij hv hw
    -- Both logs are log 0...
    have h1 := hv.1
    -- ...on both sides...
    have h2 := hw.1
    -- ...yet `i < j`: impossible.
    omega
  -- No log numbered 1 or above holds anything, so the rule holds vacuously.
  ·
    -- Take an ingested version t and a version r in an unflushed log j.
    intro t r j _ hj hr _
    -- The log holding `r` is log 0...
    have := hr.1
    -- ...yet it is numbered 1 or above: impossible.
    omega
  -- `r = (0, 1)` from log 0 and `t = (0, 2)` from the ingest.
  ·
    -- The two versions, where each lives, that they share key 0, and one fact left: r is older than
    -- t.
    refine ⟨⟨0, 1⟩, ⟨0, 2⟩, ⟨0, Nat.le_refl 0, rfl, rfl⟩, Or.inr rfl, rfl, ?_⟩
    -- `2 < 1` is false.
    decide

/-! ## Part 3: flushes from several threads (group commit meets E30) -/

/-- What recovery brings back when the logs `flushed` are in tables and the
last synced table batch recorded `min_wal_id = minWal`: every version of a
flushed log, from its table, and every version of a log at or above
`minWal`, replayed. Mirrors `MemOf` and `TableWritesOf` in the TLA+ model. -/
def Recovered (logs : Logs) (flushed : Nat → Prop) (minWal : Nat) (v : Version) : Prop :=
  -- In a table, or replayed
  (∃ j, flushed j ∧ logs j v) ∨ Replayed logs minWal v

/-- **Oldest first keeps every log.** Every flush path runs
`Flusher::flush_oldest` under the `flushing` exclusion and takes the front of
the queue, so the flushed logs are always the oldest ones, `0 .. f - 1`, and
the last table's batch records `min_wal_id = f`. Then every version of every
log comes back, from a table or replayed: the flush half of `AckedSurvive`
with two memtables waiting (`MC_ManifestRecovery_Green_Frozen2`). -/
theorem oldest_first_keeps_every_log {logs : Logs} {f : Nat} :
    -- Claim: any version of any log comes back
    ∀ j v, logs j v → Recovered logs (fun i => i < f) f v := by
  -- Take a version `v` that log `j` holds.
  intro j v hv
  -- Either log `j` is one of the flushed ones, or it is at or above `f`.
  by_cases h : j < f
  -- Flushed: its table holds `v`.
  ·
    -- The first alternative: log j is flushed and holds v.
    exact Or.inl ⟨j, h, hv⟩
  -- Not flushed: it is at or above `min_wal_id = f`, so it is replayed.
  ·
    -- The second alternative: log j is at or above min_wal_id and holds v.
    exact Or.inr ⟨j, by omega, hv⟩

/-- Two logs waiting for a flush: log 0 holds key 0 at sequence 1, log 1 holds
key 1 at sequence 2. -/
def twoLogs : Logs := fun i v => (i = 0 ∧ v = ⟨0, 1⟩) ∨ (i = 1 ∧ v = ⟨1, 2⟩) -- log 0 holds (0, 1), log 1 holds (1, 2)

/-- **RED `NewestFirst`.** A flush takes log 1, the newer, first and records
`min_wal_id = 2`; the power goes before log 0 is flushed. Log 0's version is
in no table and is not replayed: an acknowledged write is lost
(`MC_ManifestRecovery_Red_NewestFirst`). -/
theorem newest_first_loses_a_log :
    -- Log 0 holds `(0, 1)`, and it does not come back.
    twoLogs 0 ⟨0, 1⟩ ∧ ¬ Recovered twoLogs (fun i => i = 1) 2 ⟨0, 1⟩ := by -- the two facts together
  -- Two claims, proved one by one.
  constructor
  -- Log 0 holds `(0, 1)`: the left case of `twoLogs`.
  ·
    -- By the definition of the two logs.
    exact Or.inl ⟨rfl, rfl⟩
  -- Suppose it came back, from a table or replayed.
  ·
    -- Two cases, each naming the log j it came from.
    rintro (⟨j, hj, hv⟩ | ⟨j, hj, hv⟩)
    -- From a table: the only flushed log is log 1.
    ·
      -- Replace j by 1.
      subst hj
      -- Log 1 holds only `(1, 2)`, which is not `(0, 1)`.
      rcases hv with ⟨h, _⟩ | ⟨_, h⟩
      -- `1 = 0` is false.
      ·
        -- Settled by computation.
        exact absurd h (by decide)
      -- `(0, 1) = (1, 2)` is false: the keys differ.
      ·
        -- Compare the keys of the two versions: 0 and 1.
        exact absurd (congrArg Version.key h) (by decide)
    -- Replayed: the log is numbered 2 or more, and no such log holds anything.
    ·
      -- Either way the log would be numbered 0 or 1, yet it is 2 or more.
      rcases hv with ⟨h, _⟩ | ⟨h, _⟩ <;> omega

-- End of the namespace opened at the top.
end Regolith.ManifestRecovery
