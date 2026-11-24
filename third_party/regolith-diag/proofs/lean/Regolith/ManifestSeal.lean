/-!
# ManifestSeal: a sealed manifest keeps its checksum (plan 4.12, D45)

The story. The MANIFEST is the log of version edits: which tables exist,
the last sequence, the next file id. On an encrypted database each edit
batch is sealed with AES-256-GCM-SIV under the key the provider names
current, and names that key (`src/engine/manifest/sealed.rs`). Picture two
batches: the first synced, the second in the page cache when the power
goes. The second comes back with its length but zeros for its bytes. The
reopen must drop it and open with the first. But a batch sealed under a key
the provider lacks, or opened under a wrong key, also fails its tag. If the
tag were the only check, nothing could tell the two apart: drop both, and a
wrong key opens a database with no tables; refuse both, and a plain power
cut leaves a database nobody can open.

The engine keeps the checksum, which needs no key, and checks it first
(`VersionSet::replay_manifest`): a batch whose checksum fails is torn and
ends the replay; a batch whose checksum holds is opened under the key it
names, and a missing key or a failed tag refuses.

This file backs `proofs/tla/ManifestSeal.tla` (configurations
`MC_ManifestSeal_*`). What is proved, for manifests of every length:

1. `replay_opens_with_right_keys`: with the right keys, the replay of any
   crash that kept the synced batches opens and keeps them all
   (`RecoveryOpens`, `KeepsSynced`).
2. `open_ends_only_at_a_torn_batch`: when the replay opens and stops
   before the last batch, the batch it stopped at was torn: a whole batch
   it could not read never ends it (`OnlyTornEnds`).
3. `wrong_key_refuses` and `missing_key_refuses`: a whole batch under a
   wrong or missing key refuses the open.
4. The RED cases as counterexamples: `tag_only_stop_opens_under_a_wrong_key`
   and `tag_only_refuse_refuses_a_power_cut`.
-/

-- Everything below lives in its own namespace.
namespace Regolith.ManifestSeal

/-- What a power cut left of one edit batch, the frame
`[len u32][key id][nonce][sealed edits][tag][checksum u32]` of
`src/engine/manifest.rs`. -/
inductive Fate where
  /-- Every byte the writer wrote is on disk. -/
  | whole
  /-- The length is on disk but the bytes after it are not: the checksum
  fails. -/
  | zeros
  /-- The file ends inside the batch: the length says more than is left. -/
  | cut
  -- Two fates can be compared, so `decide` can check small examples.
  deriving DecidableEq

/-- One batch as the reopen finds it: what the crash left, and the key id
written beside its nonce. -/
structure Batch where
  /-- What the crash left of it. -/
  fate : Fate
  /-- The key id it names (`KeyId`). -/
  key : Nat

/-- The key provider at the reopen (`KeyProvider`). -/
structure Provider where
  /-- Whether it hands back any key under id `k` (`KeyProvider::key`). -/
  has : Nat → Bool
  /-- Whether the keys it hands back are the bytes that sealed the batches. -/
  right : Bool

/-- Whether a batch's checksum holds. It takes no key: it only asks
whether the bytes are the ones written. -/
def checksumHolds (b : Batch) : Bool :=
  -- Whole bytes pass; zeros or a cut batch fail.
  b.fate == .whole

/-- Whether a batch's tag holds: whole bytes, a key under the id it names,
and that key the one that sealed it. -/
def tagHolds (p : Provider) (b : Batch) : Bool :=
  -- All three must hold at once.
  b.fate == .whole && p.has b.key && p.right

/-- The engine's replay: how many batches the open keeps, or `none` when it
refuses. -/
def replay (p : Provider) : List Batch → Option Nat
  -- No batch left: the replay read them all and keeps none more.
  | [] => some 0
  -- The next batch, then the rest.
  | b :: rest =>
    -- A failed checksum is a torn batch: the replay ends here.
    if !checksumHolds b then some 0
    -- A whole batch that will not open under its key refuses.
    else if !tagHolds p b then none
    -- A batch that opens is kept, and the replay goes on.
    else (replay p rest).map (· + 1)

/-- The defect `TagOnlyStop`: no checksum, and a batch whose tag fails
ends the replay as a torn one would. -/
def replayTagOnlyStop (p : Provider) : List Batch → Option Nat
  -- No batch left: keep none more.
  | [] => some 0
  -- The next batch, then the rest.
  | b :: rest =>
    -- Any batch it cannot open ends the replay.
    if !tagHolds p b then some 0
    -- One it can open is kept, and the replay goes on.
    else (replayTagOnlyStop p rest).map (· + 1)

/-- The defect `TagOnlyRefuse`: no checksum, and a whole-length batch whose
tag fails refuses as a wrong key would. -/
def replayTagOnlyRefuse (p : Provider) : List Batch → Option Nat
  -- No batch left: keep none more.
  | [] => some 0
  -- The next batch, then the rest.
  | b :: rest =>
    -- Only a batch the file ends inside reads as torn.
    if b.fate == .cut then some 0
    -- Any other batch it cannot open refuses.
    else if !tagHolds p b then none
    -- One it can open is kept, and the replay goes on.
    else (replayTagOnlyRefuse p rest).map (· + 1)

/-- **The right keys open, and keep every synced batch.** The crash kept
the synced batches `pre` whole, whatever it did to the rest; the provider
has every key the file names, with the right bytes. Then the replay opens,
keeping at least `pre`. It rules out a plain power cut refusing the open,
and a replay that stops before a synced batch. -/
theorem replay_opens_with_right_keys (p : Provider)
    -- The provider's keys are the right bytes.
    (hright : p.right = true)
    -- What the crash left: the synced batches, then the rest.
    (pre rest : List Batch)
    -- Every synced batch is whole.
    (hpre : ∀ b ∈ pre, b.fate = .whole)
    -- The provider has every key the file names.
    (hkeys : ∀ b ∈ pre ++ rest, p.has b.key = true) :
    -- The statement: the replay opens the file, and keeps at least as many batches as were synced.
    ∃ kept, replay p (pre ++ rest) = some kept ∧ pre.length ≤ kept := by
  -- Walk the synced batches one at a time.
  induction pre with
  -- No synced batch: show the replay of `rest` opens at all.
  | nil =>
    -- Walk `rest` one batch at a time, keeping its key fact as we go.
    simp only [List.nil_append, List.length_nil, Nat.zero_le, and_true] at hkeys ⊢
    -- Prove "it opens" for every list whose keys the provider has.
    induction rest with
    -- An empty file opens, keeping nothing.
    | nil => exact ⟨0, rfl⟩
    -- One more batch at the front.
    | cons b rest ih =>
      -- Its key is provided, and so are the rest's.
      have hb : p.has b.key = true := hkeys b (List.mem_cons_self ..)
      -- The rest opens, by the step before.
      obtain ⟨k, hk⟩ := ih (fun c hc => hkeys c (List.mem_cons_of_mem _ hc))
      -- Two cases: the batch is whole, or the crash tore it.
      by_cases hw : b.fate = .whole
      -- Whole: its checksum and tag hold, so it is kept and the rest opens.
      · exact ⟨k + 1, by simp [replay, checksumHolds, tagHolds, hw, hb, hright, hk]⟩
      -- Torn: the replay ends there and opens.
      · exact ⟨0, by simp [replay, checksumHolds, hw]⟩
  -- One more synced batch at the front of the synced ones.
  | cons b pre ih =>
    -- It is whole, since it was synced.
    have hw : b.fate = .whole := hpre b (List.mem_cons_self ..)
    -- Its key is provided.
    have hb : p.has b.key = true := hkeys b (List.mem_cons_self ..)
    -- The rest of the file opens keeping the rest of the synced batches.
    obtain ⟨k, hk, hlen⟩ := ih (fun c hc => hpre c (List.mem_cons_of_mem _ hc))
      -- (and the provider has the key of every later batch).
      (fun c hc => hkeys c (List.mem_cons_of_mem _ hc))
    -- So this batch is kept too, one more than the rest keeps.
    refine ⟨k + 1, ?_, ?_⟩
    -- Its checksum and tag hold, so replay keeps it and adds the rest's count.
    · simp [replay, checksumHolds, tagHolds, hw, hb, hright, hk]
    -- One more synced batch, one more kept.
    · simp only [List.length_cons]
      -- The rest kept at least its synced batches, so add one to both sides.
      omega

/-- **An open ends only at a torn batch.** When the replay opens keeping
`kept` batches and the file holds a batch at position `kept`, that batch's
checksum fails: the replay never stops at a whole batch it could not read.
It rules out a wrong or missing key reading as a shorter manifest. -/
theorem open_ends_only_at_a_torn_batch (p : Provider) :
    -- For every file and every count the replay could keep...
    ∀ (bs : List Batch) (kept : Nat),
      -- ...if the replay opened keeping that many...
      replay p bs = some kept →
      -- ...then the batch right after the kept ones, if there is one...
      ∀ b, bs[kept]? = some b →
        -- ...is torn.
        checksumHolds b = false := by
  -- Walk the file one batch at a time.
  intro bs
  -- Prove it for every file, from the shortest up.
  induction bs with
  -- An empty file has no batch after the kept ones.
  | nil =>
    -- Any position in an empty list holds nothing.
    intro kept _ b hb
    -- So the case cannot happen.
    simp at hb
  -- One batch at the front, then the rest.
  | cons c rest ih =>
    -- Take the kept count and the replay's verdict.
    intro kept hreplay b hb
    -- Two cases: the front batch is torn, or whole.
    by_cases hc : checksumHolds c = true
    -- Whole: the replay either refused (not this case) or kept it.
    · cases htag : tagHolds p c with
      -- The tag failed: the replay refused, contradicting that it opened.
      | false => simp [replay, hc, htag] at hreplay
      -- The tag held: the replay kept it and opened on the rest.
      | true =>
        -- Unfold one replay step: it kept the rest's count plus one.
        simp only [replay, hc, htag, Bool.not_true, Bool.false_eq_true, ite_false] at hreplay
        -- Name the rest's count.
        obtain ⟨k, hk, rfl⟩ := Option.map_eq_some_iff.mp hreplay
        -- Position k + 1 in this file is position k in the rest.
        simp only [List.getElem?_cons_succ] at hb
        -- The step for the rest says that batch is torn.
        exact ih k hk b hb
    -- Torn: the replay ended at it, keeping nothing.
    · simp only [Bool.not_eq_true] at hc
      -- So it kept 0 batches.
      simp only [replay, hc, Bool.not_false, ite_true, Option.some.injEq] at hreplay
      -- Position 0 is the torn front batch.
      subst hreplay
      -- That batch is `c`.
      simp only [List.getElem?_cons_zero, Option.some.injEq] at hb
      -- And its checksum fails.
      exact hb ▸ hc

/-- **A wrong key refuses.** A whole batch under a provider whose bytes are
not the ones that sealed it refuses the open, whatever follows. -/
theorem wrong_key_refuses (p : Provider) (b : Batch) (rest : List Batch)
    -- The provider's keys are the wrong bytes.
    (hwrong : p.right = false)
    -- The batch is whole.
    (hw : b.fate = .whole) :
    -- The statement: the replay refuses the file.
    replay p (b :: rest) = none := by
  -- Its checksum holds and its tag fails, so the replay refuses at it.
  simp [replay, checksumHolds, tagHolds, hw, hwrong]

/-- **A missing key refuses.** A whole batch naming a key the provider does
not have refuses the open (`Error::UnknownKey`), whatever follows. -/
theorem missing_key_refuses (p : Provider) (b : Batch) (rest : List Batch)
    -- The provider has no key under the id the batch names.
    (hmissing : p.has b.key = false)
    -- The batch is whole.
    (hw : b.fate = .whole) :
    -- The statement: the replay refuses the file.
    replay p (b :: rest) = none := by
  -- Its checksum holds and its tag cannot, so the replay refuses at it.
  simp [replay, checksumHolds, tagHolds, hw, hmissing]

/-- **RED `tag_only_stop_opens_under_a_wrong_key`.** One synced, whole
batch; the provider has the key id but other bytes. The engine refuses.
`TagOnlyStop` reads the failed tag as a torn batch and opens keeping
nothing: every table the manifest named is lost without a word. -/
theorem tag_only_stop_opens_under_a_wrong_key :
    -- The engine refuses...
    replay ⟨fun _ => true, false⟩ [⟨.whole, 1⟩] = none ∧
    -- ...the defect opens with no batch.
    replayTagOnlyStop ⟨fun _ => true, false⟩ [⟨.whole, 1⟩] = some 0 :=
  -- Both sides are one replay step Lean can evaluate.
  ⟨rfl, rfl⟩

/-- **RED `tag_only_refuse_refuses_a_power_cut`.** Two batches under the
right key; the first synced, the second torn by the power cut with its
length intact. The engine opens keeping the first. `TagOnlyRefuse` cannot
tell the torn batch from a wrong key and refuses: a plain power cut leaves
a database nobody can open. -/
theorem tag_only_refuse_refuses_a_power_cut :
    -- The engine opens with the synced batch...
    replay ⟨fun _ => true, true⟩ [⟨.whole, 1⟩, ⟨.zeros, 1⟩] = some 1 ∧
    -- ...the defect refuses.
    replayTagOnlyRefuse ⟨fun _ => true, true⟩ [⟨.whole, 1⟩, ⟨.zeros, 1⟩] = none :=
  -- Both sides are two replay steps Lean can evaluate.
  ⟨rfl, rfl⟩

-- The end of the namespace.
end Regolith.ManifestSeal
