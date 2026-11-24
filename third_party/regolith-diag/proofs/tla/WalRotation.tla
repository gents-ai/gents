---- MODULE WalRotation ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* Before a write counts, the database notes it in a file called the
\* write-ahead log, one record per commit, so that after a power cut it can
\* read the log back ("replay") and lose nothing it promised to keep. The
\* log is not one file but a row of files: when the newest file has served
\* its memtable, it is "sealed" and a new file is opened (a "rotation").
\*
\* Writing to a file does not put the bytes on the disk at once; only a
\* "sync" does. A power cut keeps everything synced and may keep any front
\* part of what was written after the last sync, possibly ending in half a
\* record (a "torn" record). Replay expects a torn record only at the very
\* end of the NEWEST file, where it simply stops. A torn record in an older
\* file looks like damage, and the database refuses to open.
\*
\* Tiny example of what can go wrong. Commits 1 and 2 are written to file 1
\* but not synced. A rotation opens file 2, and commit 3 is written and
\* synced there. The power is cut. File 1 lost commit 2 (or kept half of
\* it); file 2 has commit 3. Either the database refuses to open, or it
\* opens with commits 1 and 3 but not 2: a hole in history.
\*
\* The rule: a rotation syncs the old file BEFORE the new file exists. What
\* this model shows: with the rule, after any power cut at any moment the
\* database opens and holds commits 1, 2, ..., n with no hole, including
\* every synced one; without it, both failures above happen. (E2.)
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/WalRecovery.lean:
\*   step_inv, reachable_inv        a rotation that syncs first leaves every
\*                                  sealed file fully durable
\*   recovers_prefix,               after any power cut recovery succeeds
\*   reachable_recovers_prefix      with a gap-free prefix of commit order
\*                                  holding every durable record
\*   no_sync_breaks_recovery        the RED case, as a counterexample
\* TLC checks the same invariants here over every interleaving of appends,
\* syncs and rotations, against every power cut at every state.
\*
\* THE ENGINE.
\*   Wal::append_group, Wal::sync_data   src/engine/wal.rs
\*     A commit group appends its records to the newest log file; a sync
\*     makes every record in that file durable.
\*   seal_active, swap_wal          src/engine/mod.rs
\*     A rotation seals the newest log file and opens the next one:
\*     swap_wal syncs the old log, creates the new file, then swaps, all
\*     under the active_wal lock so no commit appends in between.
\*   WalReplayIter, WalPosition     src/engine/wal_replay.rs
\*     A torn record ends the newest file. In an earlier file it is damage,
\*     and recovery refuses to open.
\*
\* THE DEFECT (Rotation = "NoSync"). The sealed file is not synced before the
\* next file opens. A power cut can then tear a record in the sealed file,
\* which is no longer the newest, and recovery refuses; or cut the sealed
\* file at a record boundary while the newer file keeps its records, and
\* recovery yields later commits without the earlier ones: a gap.
\*
\* THE FIX (Rotation = "SyncFirst"). The rotation syncs the sealed file, and
\* only then opens the new file, so the new file takes a record only after
\* the sealed one is durable. The order matters even with no record in the
\* new file yet: replay ranks files by name, and an empty newer file makes an
\* unsynced sealed file an earlier one, whose torn tail it refuses.
\*
\* Commit numbers start at 1 here and at 0 in Lean; nothing else differs.
\*
\* CONFIGURATIONS.
\*   MC_WalRotation_Green           Rotation = "SyncFirst"
\*     RecoversPrefix and KeepsSynced hold.
\*   MC_WalRotation_Red_NoSync      Rotation = "NoSync"
\*     RecoversPrefix fails: the first break is a refusal.
\*   MC_WalRotation_Red_NoSync_Gap  Rotation = "NoSync"
\*     NoGap fails: a cut that keeps recovery open loses earlier commits.

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The largest number of commits the model appends.
  MaxRecords,
  \* The largest number of log files the model opens.
  MaxFiles,
  \* "SyncFirst" (the engine: RegolithEngine::swap_wal syncs the old log before it creates the next)
  \* or "NoSync" (the planted bug, E2).
  Rotation

\* The two bounds are numbers, and at least one file may exist.
ASSUME MaxRecords \in Nat /\ MaxFiles \in Nat \ {0}
\* The rotation rule is one of the two.
ASSUME Rotation \in {"SyncFirst", "NoSync"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The log files, oldest first. Each is the list of commit numbers written to it, in order (one
  \* Wal per file, src/engine/wal.rs; the files are named by a growing id, wal_filename).
  files,
  \* For each file, how many of its records, counted from its start, a sync has put safely on disk.
  \* A power cut cannot lose those.
  synced,
  \* How many commits were appended so far.
  next

\* Every variable, so a step that changes none of them is a stutter.
vars == <<files, synced, next>>

\* The position of the newest file.
Last == Len(files)

\* The set of elements of a sequence.
Range(s) == {s[j] : j \in 1..Len(s)}

\* The start: one empty log file, nothing synced, no commit.
Init ==
  \* One log file, holding nothing.
  /\ files  = <<<<>>>>
  \* Nothing in it is synced.
  /\ synced = <<0>>
  \* No commit yet.
  /\ next   = 0

\* A commit appends its record, the next commit number, to the newest file.
AppendRecord ==
  \* The model may still append.
  /\ next < MaxRecords
  \* The next commit number is added at the end of the newest file.
  /\ files' = [files EXCEPT ![Last] = Append(@, next + 1)]
  \* One more commit was appended.
  /\ next'  = next + 1
  \* Nothing is synced by an append.
  /\ UNCHANGED synced

\* A sync of the newest file makes every record in it durable.
Sync ==
  \* The newest file holds records that are not on disk yet.
  /\ synced[Last] < Len(files[Last])
  \* Now every record in it is.
  /\ synced' = [synced EXCEPT ![Last] = Len(files[Last])]
  \* Nothing else changes.
  /\ UNCHANGED <<files, next>>

\* A rotation seals the newest file and opens an empty one. The fix syncs
\* the sealed file first; the defect does not.
Rotate ==
  \* One more file may be opened.
  /\ Len(files) < MaxFiles
  \* The newest file holds something (an empty log is not rotated).
  /\ files[Last] # <<>>
  \* A new, empty file becomes the newest.
  /\ files'  = Append(files, <<>>)
  \* The synced counts gain one entry, 0, for the new file. Before that, the engine's rule
  /\ synced' = Append(IF Rotation = "SyncFirst"
                        \* marks every record of the sealed file as on disk;
                        THEN [synced EXCEPT ![Last] = Len(files[Last])]
                        \* the planted bug leaves the sealed file as it was.
                        ELSE synced,
                      0)
  \* No commit is appended by a rotation.
  /\ UNCHANGED next

\* Every step the system can take.
Next == AppendRecord \/ Sync \/ Rotate

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Power cuts and replay. A cut of one file is <<kept, torn>>: how many whole
\* records survive, and whether part of one more record follows them.

\* What a power cut may leave of file i: everything synced survives, at most
\* every record survives, and a torn record follows only if one was written.
CutsOf(i) ==
  \* The pairs <<kept, torn>> where kept is anything from the synced count up to all the file's
  \* records, and a torn record may follow only when not all records were kept.
  {c \in (synced[i]..Len(files[i])) \X BOOLEAN : c[2] => c[1] < Len(files[i])}

\* Every power cut of the whole log: one cut per file.
Cuts ==
  \* The lists of one cut per file
  {c \in [1..Len(files) -> UNION {CutsOf(i) : i \in 1..Len(files)}] :
     \* (each file's cut must be one the file allows).
     \A i \in 1..Len(files) : c[i] \in CutsOf(i)}

\* WalPosition::Earlier: replay refuses when a file other than the newest
\* ends in a torn record.
Refused(c) == \E i \in 1..(Len(files) - 1) : c[i][2]

\* The records replay yields from file i on, in file order. A torn tail
\* contributes nothing: it ends the newest file, and an earlier one refuses.
RECURSIVE Replayed(_, _)
\* Walk the files from i on.
Replayed(c, i) ==
  \* Past the last file: nothing.
  IF i > Len(files) THEN <<>>
  \* This file's kept records, followed by what the later files yield.
  ELSE SubSeq(files[i], 1, c[i][1]) \o Replayed(c, i + 1)

\* A gap-free prefix of commit order: commits 1, 2, ..., n.
IsPrefix(s) == s = [j \in 1..Len(s) |-> j]

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* Between one file and the allowed number exist.
  /\ Len(files) \in 1..MaxFiles
  \* There is one synced count per file.
  /\ Len(synced) = Len(files)
  \* A synced count never exceeds its file's records.
  /\ \A i \in 1..Len(files) : synced[i] \in 0..Len(files[i])
  \* The commit count stays within the bound.
  /\ next \in 0..MaxRecords

\* Recovery opens after any power cut at this moment: no cut leaves a torn
\* record in a file other than the newest.
\* Rules out, for example: a rotation seals file 1 without syncing it; the power is cut and tears
\* the last record of file 1 in half; on restart the damaged record sits in a file that is not the
\* newest, and the database refuses to open.
RecoveryOpens == \A c \in Cuts : ~Refused(c)

\* Whatever recovery yields when it opens is commits 1..n: no commit is
\* recovered without every commit before it.
\* Rules out, for example: commits 1 and 2 are in an unsynced, sealed file and commit 3 in the next
\* file; a power cut loses 1 and 2 but keeps 3; the database opens holding commit 3 without the two
\* commits it was built on.
NoGap == \A c \in Cuts : ~Refused(c) => IsPrefix(Replayed(c, 1))

\* THE HEADLINE. After a power cut at this moment, whatever it left of each
\* file, recovery succeeds and yields a gap-free prefix of commit order.
\* Lean: recovers_prefix.
RecoversPrefix == RecoveryOpens /\ NoGap

\* Recovery also keeps every record a sync made durable.
\* Lean: recovers_prefix, its bound durable w <= m.
KeepsSynced ==
  \* For every power cut after which recovery opens:
  \A c \in Cuts : ~Refused(c) =>
    \* every record a sync put on disk is among the recovered ones. Rules out a commit that was
    \* reported durable and is gone after the restart.
    \A i \in 1..Len(files) : \A j \in 1..synced[i] : files[i][j] \in Range(Replayed(c, 1))

====
