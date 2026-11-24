---- MODULE ManifestSeal ----
\* A sealed MANIFEST across a power cut and a reopen (plan 4.12, D45).
\*
\* THE STORY. The MANIFEST is the log of version edits: which tables exist,
\* the last sequence, the next file id. On an encrypted database every edit
\* batch is sealed with AES-256-GCM-SIV under the key the provider names
\* current, and names that key (src/engine/manifest/sealed.rs). Picture two
\* batches: the first synced, the second still in the page cache when the
\* power goes. The second comes back torn: its length is there, its bytes
\* are zeros. On reopen the replay must drop it, as it drops any torn
\* batch, and open with the first. But a batch sealed under a key the
\* provider no longer has, or opened under a wrong key, also fails its tag.
\* If the tag were the only check, the replay could not tell the two apart:
\* drop both and a wrong key opens an empty database, every table lost;
\* refuse both and a plain power cut leaves a database nobody can open.
\*
\* THE ENGINE KEEPS THE CHECKSUM. Each batch is
\*   [len u32][key id u32][nonce][sealed edits][tag][checksum u32]
\* and the checksum covers everything between len and itself. Replay
\* (VersionSet::replay_manifest) checks it first, without any key: a batch
\* whose checksum fails is torn and ends the replay, as in format 1. Only a
\* batch whose checksum holds is opened, under the key it names: a key the
\* provider lacks refuses (Error::UnknownKey), a tag that fails refuses
\* (wrong key or tampering). A batch the file ends inside (its length says
\* more than the file holds) is torn too.
\*
\* WHAT IS CHECKED.
\*   RecoveryOpens  the right keys open after any honest power cut.
\*   KeepsSynced    and keep every batch a completed sync covered.
\*   OnlyTornEnds   an open ends only at a torn batch, never at a whole one
\*                  it could not read: a wrong or missing key never reads as
\*                  an empty or shorter manifest.
\*   OldKeyRetired  once a rewrite under the current key replaced every
\*                  batch, a provider without the old key opens.
\*
\* THE DEFECTS, one per Mutant value.
\*   "TagOnlyStop"    no checksum; a batch whose tag fails ends the replay.
\*                    A wrong key ends it at the first batch: the database
\*                    opens with no table. OnlyTornEnds fails.
\*   "TagOnlyRefuse"  no checksum; a batch whose tag fails refuses. A torn
\*                    unsynced batch refuses the open. RecoveryOpens fails.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - The edits inside a batch: replay applies a batch whole or not at all,
\*     so a batch is one step here.
\*   - The salt and offset the tag binds: a batch copied from elsewhere
\*     fails its tag like a wrong key, which the model already covers.
\*   - Unsealed manifests: a database opened with a provider rewrites one
\*     sealed at its first read-write open, which is the Rewrite step.
\*
\* CONFIGURATIONS.
\*   MC_ManifestSeal_Green                 every invariant holds
\*   MC_ManifestSeal_Red_TagOnlyStop       OnlyTornEnds fails
\*   MC_ManifestSeal_Red_TagOnlyRefuse     RecoveryOpens fails

\* Naturals for counting, Sequences for the log of batches.
EXTENDS Naturals, Sequences

\* The knobs of a run. Each configuration file (MC_ManifestSeal_*.cfg) gives them values.
CONSTANTS
  \* The most batches the manifest holds at once: bounds the model.
  MaxBatches,
  \* "none" for the engine, or one defect named above.
  Mutant

\* There must be room for at least one batch.
ASSUME MaxBatches \in Nat \ {0}
\* Only the design and the two defects.
ASSUME Mutant \in {"none", "TagOnlyStop", "TagOnlyRefuse"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The batches as written, oldest first. Each is [key |-> k]: the key id
  \* it was sealed under, which the engine writes in clear beside the
  \* nonce (manifest/sealed.rs, encode_batch; VersionSet::sealed_under).
  log,
  \* How many leading batches a completed sync made durable
  \* (VersionSet::apply syncs every batch that adds or removes a table).
  synced,
  \* The key id the provider names current (KeyProvider::current): 1 or 2.
  current

\* Every variable, so a step that changes none of them is a stutter.
vars == <<log, synced, current>>

\* The two key ids the model rotates between.
Keys == {1, 2}

----------------------------------------------------------------------------
\* The writer's steps.

\* The database starts with an empty sealed manifest, key 1 current.
Init ==
  \* No batch yet: the stamp alone (its own sync is the open's).
  /\ log = <<>>
  \* Nothing to have synced.
  /\ synced = 0
  \* The provider starts with key 1.
  /\ current = 1

\* An edit batch is appended, sealed under the current key
\* (VersionSet::apply, encode_records).
AppendBatch ==
  \* Only while the model has room.
  /\ Len(log) < MaxBatches
  \* The new batch names the key current right now.
  /\ log' = Append(log, [key |-> current])
  \* The sync and the key are not touched.
  /\ UNCHANGED <<synced, current>>

\* A sync completes: every batch written so far is durable.
Sync ==
  \* Only when there is something new to sync.
  /\ synced < Len(log)
  \* Everything in the file is now on disk.
  /\ synced' = Len(log)
  \* The batches and the key stay as they are.
  /\ UNCHANGED <<log, current>>

\* The provider rotates: new batches use the other key.
Rotate ==
  \* 1 becomes 2 and 2 becomes 1.
  /\ current' = 3 - current
  \* Nothing written changes.
  /\ UNCHANGED <<log, synced>>

\* The manifest is rewritten: one batch under the current key replaces
\* them all (VersionSet::compact_manifest). The new file is written to a
\* staging name, synced and renamed, so it is durable as one step.
Rewrite ==
  \* Something to rewrite.
  /\ Len(log) > 0
  \* One batch, sealed under the key current now.
  /\ log' = <<[key |-> current]>>
  \* Durable at once: the rename lands only after the sync.
  /\ synced' = 1
  \* The key is the same one.
  /\ UNCHANGED current

\* Every step the writer can take.
Next == AppendBatch \/ Sync \/ Rotate \/ Rewrite

\* Start at Init, then take steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* What a power cut leaves, and how a reopen reads it.
\*
\* A crash state is a record:
\*   st    [1..Len(log) -> fate]: "whole" (the bytes the writer wrote),
\*         "zeros" (length there, bytes not: the checksum fails), "cut"
\*         (the file ends inside the batch) or "absent";
\*   prov  the provider at the reopen: "right" (every key id, right bytes),
\*         "wrong" (every key id, other bytes) or "noOld" (key 2 only).

\* Every crash state now. L is how many batches the file still holds,
\* at least the synced ones.
CrashStates ==
  \* n: how many batches were written before the power went.
  LET n == Len(log) IN
  \* Gather the crash states for each length L the file can be left with.
  UNION {
    \* One crash state for every fate choice and provider, at this L.
    {[st |-> [i \in 1..n |->
               \* A synced batch is on disk as written.
               IF i <= synced THEN "whole"
               \* An unsynced batch still in the file takes the chosen fate.
               ELSE IF i <= L THEN f[i]
               \* The rest never reached the disk.
               ELSE "absent"],
      \* The provider the database is reopened with.
      prov |-> p] :
       \* f: a fate for each unsynced batch still in the file...
       f \in {g \in [(synced + 1)..L -> {"whole", "zeros", "cut"}] :
                \* ...where only the last one can be cut short.
                \A i \in (synced + 1)..L : g[i] = "cut" => i = L},
       \* Any of the three providers.
       p \in {"right", "wrong", "noOld"}} :
    \* The file keeps at least the synced batches and at most all of them.
    L \in synced..n}

\* How many batches the file holds after the crash: they are a prefix.
Held(cs) == {i \in DOMAIN cs.st : cs.st[i] # "absent"}

\* How many that is.
Count(cs) == IF Held(cs) = {} THEN 0 ELSE CHOOSE m \in Held(cs) : \A i \in Held(cs) : i <= m

\* Whether the provider has the key id batch i names.
Provided(cs, i) ==
  \* Every key id, right or wrong bytes...
  \/ cs.prov \in {"right", "wrong"}
  \* ...or only key 2.
  \/ log[i].key = 2

\* Whether batch i's checksum holds: its bytes are the ones written. The
\* checksum takes no key.
ChecksumHolds(cs, i) == cs.st[i] = "whole"

\* Whether batch i's tag holds: the bytes are whole, the provider has the
\* key the batch names, and that key's bytes are the right ones.
TagHolds(cs, i) ==
  \* The sealed bytes are the ones written...
  /\ cs.st[i] = "whole"
  \* ...the provider has the key the batch names...
  /\ Provided(cs, i)
  \* ...and hands back the bytes that sealed it.
  /\ cs.prov # "wrong"

\* Whether the replay stops at batch i, keeping the batches before it.
Stops(cs, i) ==
  \* The engine: the file ends inside it, or its checksum fails.
  CASE Mutant = "none"          -> ~ChecksumHolds(cs, i)
       \* Tag only, stop: any batch it cannot open ends the replay.
       [] Mutant = "TagOnlyStop"   -> ~TagHolds(cs, i)
       \* Tag only, refuse: only a batch cut short ends the replay.
       [] Mutant = "TagOnlyRefuse" -> cs.st[i] = "cut"

\* Whether the replay refuses the open at batch i.
Refuses(cs, i) ==
  \* The engine: the checksum holds but the key is missing or the tag fails.
  CASE Mutant = "none"          -> ChecksumHolds(cs, i) /\ ~TagHolds(cs, i)
       \* Tag only, stop: it never refuses.
       [] Mutant = "TagOnlyStop"   -> FALSE
       \* Tag only, refuse: every whole-length batch it cannot open refuses.
       [] Mutant = "TagOnlyRefuse" -> cs.st[i] # "cut" /\ ~TagHolds(cs, i)

\* The batches where the replay stops or refuses.
Events(cs) == {i \in 1..Count(cs) : Stops(cs, i) \/ Refuses(cs, i)}

\* The first of them, or one past the last batch when the replay reads all.
End(cs) ==
  \* No event: the replay reads every batch.
  IF Events(cs) = {} THEN Count(cs) + 1
  \* Else the earliest one, which is where the replay acts.
  ELSE CHOOSE e \in Events(cs) : \A j \in Events(cs) : e <= j

\* The reopen refuses: the replay's first event is a refusal.
Refused(cs) == End(cs) <= Count(cs) /\ Refuses(cs, End(cs))

\* How many batches an open keeps: every one before the end.
Kept(cs) == End(cs) - 1

----------------------------------------------------------------------------
\* Invariants. Each quantifies over every crash state, so a power cut at
\* every step of every behaviour is checked.

\* Every variable holds what its comment says.
TypeOK ==
  \* The log is a sequence of batches naming a key.
  /\ log \in Seq([key : Keys])
  \* The sync covers only batches that exist.
  /\ synced \in 0..Len(log)
  \* The current key is one of the two.
  /\ current \in Keys

\* With the right keys, every honest power cut leaves a database that
\* opens. It rules out the TagOnlyRefuse story: two batches, the second
\* torn by the cut, and the open refusing over it. Lean:
\* replay_opens_with_right_keys.
RecoveryOpens == \A cs \in CrashStates : cs.prov = "right" => ~Refused(cs)

\* With the right keys, the open keeps every batch a sync covered. It rules
\* out a replay that stops early and silently drops a synced table. Lean:
\* replay_opens_with_right_keys, its bound.
KeepsSynced == \A cs \in CrashStates : cs.prov = "right" => Kept(cs) >= synced

\* An open ends only at a torn batch: one whose checksum fails. It rules
\* out the TagOnlyStop story: a wrong key fails the first batch's tag, the
\* replay ends there, and the database opens with no table at all. Lean:
\* open_ends_only_at_a_torn_batch.
OnlyTornEnds ==
  \* Every crash state that opens and ends before the last batch...
  \A cs \in CrashStates : ~Refused(cs) /\ End(cs) <= Count(cs)
    \* ...ends at a batch whose bytes are not the ones written.
    => ~ChecksumHolds(cs, End(cs))

\* Once no batch names key 1, a provider that dropped key 1 opens. This is
\* what a rewrite under key 2 buys: the old key can be retired.
OldKeyRetired ==
  \* When every batch on disk names key 2...
  (\A i \in 1..Len(log) : log[i].key = 2)
    \* ...a reopen with key 2 only never refuses.
    => \A cs \in CrashStates : cs.prov = "noOld" => ~Refused(cs)

====
