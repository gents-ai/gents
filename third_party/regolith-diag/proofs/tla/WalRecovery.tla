---- MODULE WalRecovery ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* Before a write counts, the database notes it in the write-ahead log, one
\* record per commit group. After a power cut it reads the log back
\* ("replay") to rebuild what it had. Writing a record does not put it on
\* the disk; only a "sync" does. So after a power cut the end of the log may
\* be missing, or cut in the middle of a record, or hold garbage: a "torn
\* tail". That is normal, and replay should just stop there.
\*
\* But damage can also sit in a part of the log that WAS synced (a failing
\* disk). Stopping there would silently throw away commits the database
\* promised to keep. Replay has to tell the two apart: "a tail that never
\* reached the disk" (drop it and say so) from "damage in what was promised"
\* (refuse to open).
\*
\* The trick (log format 2): every record carries a small stamp saying "when
\* I was written, everything up to HERE had already been synced". After a
\* crash, replay takes P, the largest such claim it can read from any record
\* that survived, and O, the place of the first unusable record.
\*
\* Tiny example. Records 1, 2, 3 were written; a sync covered 1 and 2;
\* record 3 carries the stamp "2 synced". The power is cut. If record 3 is
\* torn, then O = 3 and P is at most 2: the damage is past everything
\* proven synced, so replay drops record 3, reports it, and opens with 1 and
\* 2. If instead record 2 comes back as garbage while record 3 survives,
\* then O = 2 and P = 2: something proven synced is damaged, and replay
\* refuses to open rather than lose it in silence.
\*
\* A close that still has writes in memory (E46). Close has to put the
\* last memtable into a table. The old way sealed it like a rotation: sync
\* the whole log (tens of megabytes), create a new log, flush, close the new
\* log. The new way seals it "in place": the log stays the newest file, so
\* a torn tail there is still fine and it needs no sync; the flush writes
\* the table and, in the same synced manifest batch, the note "recovery
\* skips this log"; only then is the log removed and the next log created
\* and closed. Tiny example: commits 1 and 2 sit unsynced in log 1 at
\* close. A power cut before the manifest batch is durable finds log 1 as
\* the newest file and keeps a prefix of it; one after finds 1 and 2 in a
\* table. Creating the next log first (the RED NewLogBeforeFlush) would
\* make the unsynced log 1 an earlier file, whose torn tail refuses.
\*
\* What this model shows: over every interleaving of appends, syncs,
\* rotations, flushes, a clean close and crashes, and against every state a crash can
\* leave, the database opens after any crash an honest disk can produce,
\* opens with commits 1..n and no hole, keeps every synced commit, never
\* quietly drops something a stamp proves synced, and never opens under a
\* wrong encryption key. Each planted bug breaks one of these.
\*
\* THE RULE IN FULL.
\* 4.2 and 4.3: crash-aware WAL recovery with format 2. Every group record
\* carries `synced_through`, the end of what the last completed sync made
\* durable; a clean close syncs, appends CLOSE and syncs again. Replay of
\* the newest log takes P, the largest `synced_through` it reads (end of
\* file when a usable CLOSE ends the file), and at the first unusable
\* record, at O, refuses when O < P and otherwise drops [O, end of file),
\* truncates, and reports it. Earlier logs are complete by the rotation sync
\* (WalRotation.tla), so damage there is refused. With encryption at rest
\* (4.12, D45) the AEAD tag replaces the checksum: a record whose tag fails
\* is an unusable record under the same rule.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/WalRecovery.lean
\* (the "Format 2" part):
\*   replay2_opens_of_honest     replay opens when every surviving stamp is
\*                               honest: what it claims synced survived
\*   replay2_keeps_proven        an open never discards a record a surviving
\*                               stamp proves synced
\*   step2_inv, reachable2_inv   the format 2 writer keeps every stamp at or
\*                               below the completed sync, and CLOSE only
\*                               after a sync of everything before it
\*   honest_of_inv               so after a crash the flush honoured, P is
\*                               at most O
\*   recovers_prefix2            after any crash the device's flush honoured,
\*                               recovery opens with a gap-free prefix that
\*                               keeps every synced commit
\*   residual_is_reported        a crash that damaged synced bytes and still
\*                               opens has discarded a nonempty tail
\*   wrong_key_refuses           a stamp that fails its tag refuses
\*   drop_below_p_loses_proven, refuse_above_p_never_opens,
\*   format1_refuses_torn_tail, unsealed_stamp_drops_log
\*                               the RED cases, as counterexamples
\* TLC checks the same invariants here over every interleaving of appends,
\* syncs, rotations, a clean close and a crash with recovery, against every
\* crash state at every reachable state.
\*
\* THE ENGINE (format 2, src/engine/wal.rs, wal_frame.rs, wal_replay.rs,
\* recovery.rs).
\*   AppendRecord   Wal::append_group: one record per commit group, its
\*                  header carrying `synced_through`, the offset the last
\*                  completed Wal::sync_data covered (record i's `st`).
\*   Sync(k)        Wal::sync_data: `synced_through` moves, once the sync
\*                  returns, to the offset it began at.
\*   AppendClose    Log::close (wal_log.rs): sync every record, append
\*                  CLOSE, sync; nothing is appended after CLOSE. When no
\*                  record is left unsynced the first sync is skipped, and
\*                  the one sync after CLOSE also covers the stamp, which
\*                  a crash before it may tear (TornStamp below).
\*   Rotate         Ordered::seal (WalRotation.tla); also what a close that
\*                  failed after sealing in place does before it takes a
\*                  write again (Ordered::reopen_log).
\*   Flush          A flush of the oldest frozen memtable (flush.rs): its
\*                  table, and min_wal_id past its log, in one synced
\*                  manifest batch; then the log is removed.
\*   SealInPlace    Ordered::seal_in_place: close seals the last memtable
\*                  and keeps its log as the newest, unsynced; no commit is
\*                  decided after it.
\*   FlushInPlace   That memtable's flush, once every earlier one is done
\*                  (oldest first, drain.rs DR3): the log's commits are in a
\*                  table, and the log is removed. No log file exists now.
\*   CloseNext      Ordered::close_log after a seal in place: the next log
\*                  is created and CLOSE appended to it; a Sync makes it
\*                  durable (the merged sync of AppendClose).
\*   Crash          WalReplayIter reads P from every usable record
\*                  (wal_frame::proof_past scans past the damage), refuses
\*                  when O < P, and otherwise ends the log at O; recovery
\*                  then truncates the log at O and syncs it
\*                  (wal::truncate_durably) before the next log is created,
\*                  and reports the drop.
\* Format 1 logs written by 0.1.x are still read by the format 1 rule
\* (RED Format1): a whole record with a bad checksum is refused unless
\* every byte after it is zero, and a record the file ends inside is a
\* torn tail.
\*
\* THE STAMP UNDER CHECKSUMS. The engine writes a log's stamp when it
\* creates the log and makes it durable with the log's first sync, not
\* with a sync of its own. A crash before that first sync (synced = 0
\* here) can lose the stamp, and replay then keeps nothing of the log and
\* reports every byte it held: the outcome of the crash state in which
\* every record is unusable, which CrashStates holds whenever synced = 0.
\* Every invariant below is checked against that outcome, so the model
\* needs no state of its own for it.
\*
\* THE ENGINE UNDER AEAD (Frame = "AEAD"; src/engine/wal_seal.rs, with
\* Options::key_provider set).
\*   Usable         a sealed record is usable when its header check holds
\*                  and its AES-256-GCM-SIV tag verifies under the key the
\*                  stamp names (wal_seal::open_record); the tag's
\*                  associated data is the header check's input, so it
\*                  binds the payload to `st`, the offset and the log. The
\*                  scan past damage (wal_frame::proof_past) reads P only
\*                  from records whose tag verifies.
\*   StampKeyOk     the stamp is sealed under the log's key: a stamp whose
\*                  tag fails refuses the open, naming the file
\*                  (wal_seal::open_stamp), so a wrong key refuses and
\*                  never drops the log (WrongKeyRefuses; RED
\*                  StampNotSealed).
\*   NewestStampOk  and it is whole: the engine writes the sealed stamp to
\*                  a staging file, syncs it, renames it into place and
\*                  syncs the directory (wal_seal::create_durably) before
\*                  the log takes a record, which is the model's Init and
\*                  Rotate creating a log whose stamp is already synced. A
\*                  crash leaves no log, or one whose whole stamp is
\*                  durable (RED StampUnsynced).
\*   keyOk = FALSE  a provider handing other bytes under the key id the
\*                  stamp names; a key id it does not provide at all is
\*                  refused before any tag is checked (Error::UnknownKey).
\*
\* WHAT A CRASH LEAVES (the crash states, `CrashStates`).
\*   - Under powersafe overwrite (D6: assumed, synced groups are not padded
\*     to whole sectors), every record a completed sync covered survives.
\*   - Each record past the synced prefix independently survives intact, or
\*     is unusable (garbage; a block of zeros where the length persisted
\*     before the data, which format 1 tells apart), and the file may end
\*     after any of them, inside the last one (a partial record) or not.
\*     Persistence out of order is allowed: ext4 data=writeback, volatile
\*     device caches and the OPFS slot header all produce it (E3).
\*   - Residual = TRUE adds the residual case of 4.2 item 5: the last synced
\*     group is damaged (bit rot, or a torn write rewriting a sector that
\*     held acknowledged bytes). It must be reported, never silent.
\*   - Frame = "AEAD" adds an open under the wrong key: every tag fails.
\*
\* PROMISES (4.2 item 5).
\*   Immediate: an acknowledged commit survives any crash the device's
\*     flush honours (AckedSurvive).
\*   Eventual: recovery is a gap-free prefix of commit order (NoGap).
\*   Both: every commit a completed sync covered survives (KeepsSynced); a
\*     record a surviving stamp proves synced is never discarded
\*     (NoProvenLoss); a loss of synced data is refused or reported
\*     (LossIsReported); a wrong key never opens (WrongKeyRefuses).
\*
\* DESIGN CHOICES where the plan leaves room (recorded in the report):
\*   - P is read from every usable record in the file, including those
\*     after O. Read only from the records before O it could never exceed
\*     O (a record's stamp covers only bytes written before it), and the
\*     refusal would never fire. Format 2 framing must therefore find the
\*     records after a damaged one without trusting its length (for
\*     example fixed-size blocks); the model assumes it does.
\*   - A clean close syncs every record BEFORE it appends CLOSE. Otherwise
\*     a crash can keep CLOSE (P = end of file) and lose an unsynced record
\*     before it, and replay refuses a crash it should survive
\*     (RED CloseWithoutSync).
\*   - The truncation of a dropped tail is synced before the next log is
\*     created, so the truncated log is complete when it becomes an earlier
\*     log (RED NoTruncate shows why).
\*   - With AEAD, the stamp at the head of each log is sealed under the key
\*     and synced when the log is created, so it is always inside the
\*     synced prefix: a stamp whose tag fails is refused ("wrong key or
\*     damaged header"). Without the seal, a wrong key fails every record,
\*     P is 0, and replay would drop the whole log as a torn tail
\*     (RED StampNotSealed). Without the sync at creation, a power cut
\*     before the log's first sync can tear the stamp, and since a torn
\*     sealed stamp cannot be told from a wrong key, the open refuses a
\*     crash it must survive (RED StampUnsynced).
\*   - A group of commits is one record here, so it holds one commit; a
\*     group of several survives or is lost whole, like one commit.
\*   - THE TORN STAMP (TornStamp). A checksummed log's stamp is durable only
\*     once a sync covers it. Before that a crash may tear it, and replay
\*     reads the log as unstamped: no record of it verifies (each record's
\*     check covers the stamp's nonce), nothing of it is kept and every byte
\*     is reported (wal_replay.rs, unstamped). Every crash state below may
\*     tear an unsynced checksummed stamp. It matters for the merged close:
\*     a close that finds no record syncs only after CLOSE, so until then
\*     the stamp is unsynced; replay must drop such a log, never refuse it
\*     (RED UnstampedRefuses).
\*   - SEAL IN PLACE. A close may leave its last log unsynced because the
\*     log stays the newest file until the flush's manifest batch, durable,
\*     says recovery skips it; the next log is created only after that
\*     (RED NewLogBeforeFlush creates it first).
\*
\* CONFIGURATIONS.
\*   MC_WalRecovery_Green_Immediate          every invariant holds
\*   MC_WalRecovery_Green_Eventual           every invariant holds
\*   MC_WalRecovery_Green_Residual           damage in the last synced group
\*                                           is refused or reported
\*   MC_WalRecovery_Green_Aead               AEAD frames and a wrong key
\*   MC_WalRecovery_Red_DropBelowP           NoProvenLoss fails
\*   MC_WalRecovery_Red_RefuseAboveP         RecoveryOpens fails
\*   MC_WalRecovery_Red_Format1              RecoveryOpens fails
\*   MC_WalRecovery_Red_CloseWithoutSync     RecoveryOpens fails
\*   MC_WalRecovery_Red_NoTruncate           RecoveryOpens fails
\*   MC_WalRecovery_Red_StampNotSealed       AckedSurvive fails
\*   MC_WalRecovery_Red_StampUnsynced        RecoveryOpens fails
\*   MC_WalRecovery_Red_NewLogBeforeFlush    RecoveryOpens fails
\*   MC_WalRecovery_Red_UnstampedRefuses     RecoveryOpens fails

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The largest number of commits the model appends, across crashes.
  MaxCommits,
  \* The largest number of log files: the earlier logs plus the newest.
  MaxFiles,
  \* How many crashes, each followed by a recovery, the model performs.
  MaxCrashes,
  \* When a commit is acknowledged: "Immediate" (once a sync covers it) or "Eventual" (as soon as it
  \* is appended). DurabilityMode, src/options.rs.
  Mode,
  \* What makes a record usable: "Checksum" (its checksum matches) or "AEAD" (encryption at rest:
  \* its authentication tag verifies under the key).
  Frame,
  \* TRUE when crash states may also damage the last synced record (bit rot), which a correct device
  \* never does.
  Residual,
  \* "none" for the engine's rule, or one planted bug, named in the header.
  Mutant

\* The three bounds are numbers, and at least one file may exist.
ASSUME MaxCommits \in Nat /\ MaxFiles \in Nat \ {0} /\ MaxCrashes \in Nat
\* The durability is one of the two modes.
ASSUME Mode \in {"Immediate", "Eventual"}
\* The frame is one of the two kinds.
ASSUME Frame \in {"Checksum", "AEAD"}
\* Residual is a yes or a no.
ASSUME Residual \in BOOLEAN
\* The mutant is "none" or one of the nine planted bugs.
ASSUME Mutant \in {"none", "DropBelowP", "RefuseAboveP", "Format1",
                   "CloseWithoutSync", "NoTruncate", "StampNotSealed",
                   "StampUnsynced", "NewLogBeforeFlush", "UnstampedRefuses"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The earlier log files, oldest first. Each is a list of records [c, st, ok, dur]: the commit
  \* number (0 for a CLOSE record), the stamp, whether the bytes are usable (FALSE only for a dropped
  \* tail that was left in the file), and whether a completed sync covered them (FALSE only under
  \* the planted bug NewLogBeforeFlush).
  sealed,
  \* The newest log file: the list of records [c, st] as the writer wrote them (the active Wal,
  \* src/engine/wal.rs). `st` is the record's stamp, "synced_through": how much of the log the last
  \* completed sync had covered when the record was written.
  log,
  \* How many records of the newest log, counted from its start, a completed sync has covered (the
  \* offset Wal::sync_data last covered).
  synced,
  \* Whether a CLOSE record was appended to the newest log (Wal::close).
  closed,
  \* The last commit number appended; commits are numbered 1, 2, 3, ...
  next,
  \* How many crashes with recovery have happened.
  crashes,
  \* The commits flushed into tables, in order: their manifest batch is durable and recovery
  \* replays no log that holds them (min_wal_id, log_retirement.rs).
  tables,
  \* Whether close sealed the last memtable in place: the newest log takes no record and waits for
  \* its memtable's flush (Ordered::seal_in_place).
  inplace,
  \* Whether a newest log file exists. FALSE between the in-place flush, which removed the log,
  \* and the creation of the next one.
  haslog

\* Every variable, so a step that changes none of them is a stutter.
vars == <<sealed, log, synced, closed, next, crashes, tables, inplace, haslog>>

----------------------------------------------------------------------------
\* Helpers.

\* The least and greatest elements of a finite, nonempty set of naturals.
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
\* The greatest element of a finite, nonempty set of numbers.
Max(S) == CHOOSE x \in S : \A y \in S : y <= x

\* The set of elements of a sequence.
Range(s) == {s[i] : i \in 1..Len(s)}

\* The commit numbers of a sequence of records, in order; CLOSE has none.
RECURSIVE CommitsOf(_)
\* Walk the records in order.
CommitsOf(recs) ==
  \* None left: nothing.
  IF recs = <<>> THEN <<>>
  \* This record's commit number unless it is a CLOSE, followed by the rest's.
  ELSE (IF Head(recs).c = 0 THEN <<>> ELSE <<Head(recs).c>>) \o CommitsOf(Tail(recs))

\* The commits of the earlier logs, oldest first.
RECURSIVE SealedFrom(_)
\* Walk the earlier logs from the j-th on, joining their commits.
SealedFrom(j) == IF j > Len(sealed) THEN <<>> ELSE CommitsOf(sealed[j]) \o SealedFrom(j + 1)
\* All of them, from the first.
SealedCommits == SealedFrom(1)

\* The commits of the earlier logs that a completed sync covered: all of them in the engine.
RECURSIVE DurableFrom(_)
\* Walk the earlier logs from the j-th on, keeping each record a sync covered.
DurableFrom(j) ==
  \* Past the last earlier log: nothing.
  IF j > Len(sealed) THEN <<>>
  \* This log's synced records' commits, then the later logs'.
  ELSE CommitsOf(SelectSeq(sealed[j], LAMBDA r : r.dur)) \o DurableFrom(j + 1)

\* A gap-free prefix of commit order: commits 1, 2, ..., n.
IsPrefix(s) == s = [i \in 1..Len(s) |-> i]

\* The commits a completed sync made durable: those in tables, every earlier
\* log (synced at rotation or at the truncation after a crash) and the
\* synced prefix of the newest.
SyncedCommits ==
  \* The tables' commits, the earlier logs' synced ones, and the newest log's synced prefix.
  Range(tables) \cup Range(DurableFrom(1)) \cup Range(CommitsOf(SubSeq(log, 1, synced)))

\* The commits acknowledged to their callers. Immediate acknowledges a
\* commit once a completed sync covers it; Eventual when it is appended.
Acked ==
  \* Under Immediate: the commits a completed sync covers.
  IF Mode = "Immediate" THEN SyncedCommits
  \* Under Eventual: every commit appended, in tables, the earlier logs and the newest.
  ELSE Range(tables) \cup Range(SealedCommits) \cup Range(CommitsOf(log))

----------------------------------------------------------------------------
\* Crash states of the newest log. A crash state is a record:
\*   st         [1..Len(log) -> state] of each record: "intact", "garbage",
\*              "zero", "partial" (the file ends inside it) or "absent";
\*   keyOk      whether the key provider returned the key the log was
\*              sealed under (always TRUE with checksums);
\*   rot        whether the last synced group was damaged (Residual only);
\*   stampTorn  whether the crash tore the newest log's stamp (see
\*              StampTears);
\*   tear       whether the crash garbled a record of an earlier log that
\*              no sync covered (only NewLogBeforeFlush leaves one).

\* What an unsynced record may become. Format 2 treats every unusable
\* record alike, so one kind suffices; format 1 tells zeros and a partial
\* record from garbage.
UnsyncedKinds ==
  \* Format 1 must tell four shapes apart, so its defect gets all four.
  IF Mutant = "Format1" THEN {"intact", "garbage", "zero", "partial"}
  \* Format 2 only asks "usable or not", so two shapes cover every case.
  ELSE {"intact", "garbage"}

\* Whether a crash can tear the newest log's stamp. The engine writes a
\* sealed stamp to a staging file, syncs it, and only then renames it to
\* the log's name (src/engine/wal_seal.rs, create_durably): after a power
\* cut the log either does not exist or carries its whole stamp, so there
\* is nothing to tear. The StampUnsynced defect writes the stamp in place
\* and lets the first record's sync carry it, the way an unsealed log does;
\* until that sync (synced = 0) a power cut may leave half a stamp.
\* A checksummed stamp is written unsynced and made durable by the log's
\* first sync, so until then (synced = 0) a power cut may tear it too: the
\* torn stamp row (TornStamp in the header). No log file, no stamp.
StampTears ==
  \* With no newest log file there is no stamp to tear.
  IF ~haslog THEN {FALSE}
  \* A sealed stamp: only the defect, only before the first sync.
  ELSE IF Frame = "AEAD" THEN (IF Mutant = "StampUnsynced" /\ synced = 0 THEN BOOLEAN ELSE {FALSE})
  \* A checksummed stamp: torn or whole until the first sync covers it.
  ELSE IF synced = 0 THEN BOOLEAN
  \* Covered by a sync: whole.
  ELSE {FALSE}

\* Whether a crash can garble an earlier log: only when one holds a record
\* no sync covered, which only the NewLogBeforeFlush defect leaves.
EarlierTears ==
  \* Some earlier log holds a record no completed sync covered...
  IF \E j \in 1..Len(sealed) : \E i \in 1..Len(sealed[j]) : ~sealed[j][i].dur
    \* ...then the crash may garble it, or not.
    THEN BOOLEAN
    \* Otherwise every earlier log survives whole.
    ELSE {FALSE}

\* Every crash state at this moment. L is how many records the file keeps
\* (never fewer than the synced prefix); records past the synced prefix and
\* up to L each take an unsynced kind, a partial record only as the last;
\* the rest are absent.
CrashStates ==
  \* n: how many records the newest log held when the power went.
  LET n == Len(log) IN
  \* Collect one crash state for every combination of the choices below.
  UNION {
    \* One crash state: what became of each record, the key, rot, the stamp.
    {[st |-> [i \in 1..n |->
               \* A synced record survives, but for rot in the last synced one.
               IF i <= synced THEN (IF rot /\ i = synced THEN "garbage" ELSE "intact")
               \* An unsynced record still in the file takes its chosen kind.
               ELSE IF i <= L THEN f[i] ELSE "absent"],
      \* Whether the reopen comes with the right key.
      keyOk |-> key,
      \* Whether the last synced record rotted.
      rot |-> rot,
      \* Whether the stamp came back torn.
      stampTorn |-> torn,
      \* Whether an unsynced record of an earlier log came back garbled.
      tear |-> early] :
       \* f picks a kind for each unsynced record the file still holds...
       f \in {g \in [(synced + 1)..L -> UnsyncedKinds] :
                \* ...and only the last of them may be cut short.
                \A i \in (synced + 1)..L : g[i] = "partial" => i = L}} :
    \* The file keeps at least the synced prefix and at most every record.
    L \in synced..n,
    \* With sealed frames the reopen may come with the wrong key.
    key \in (IF Frame = "AEAD" THEN BOOLEAN ELSE {TRUE}),
    \* Rot only in the Residual configurations, and only of a synced record.
    rot \in (IF Residual /\ synced > 0 THEN BOOLEAN ELSE {FALSE}),
    \* A torn stamp where one can tear.
    torn \in StampTears,
    \* A garbled earlier log where one can be.
    early \in EarlierTears}

\* How many records the crash left in the file: they are a prefix.
PLen(cs) == Cardinality({i \in DOMAIN cs.st : cs.st[i] # "absent"})

\* A record is usable when its bytes survived and its check verifies.
\* Under AEAD the tag is the check: the engine opens the record's sealed
\* payload under the key the stamp names (wal_seal::open_record), and a
\* wrong key fails every tag exactly as damage fails it.
Usable(cs, i) ==
  \* The bytes are the ones the writer wrote...
  /\ cs.st[i] = "intact"
  \* ...with sealed frames, the key is the one that sealed them...
  /\ (Frame = "Checksum" \/ cs.keyOk)
  \* ...and with checksums, the stamp survived: each record's check covers the stamp's nonce, so
  \* past a torn stamp no record verifies.
  /\ ~(Frame = "Checksum" /\ cs.stampTorn)

\* The key half of a stamp's check: a sealed stamp's tag holds only under
\* the key that sealed it (wal_seal::open_stamp).
StampKeyOk(cs) ==
  \* A checksummed log has no key to get wrong.
  \/ Frame = "Checksum"
  \* A sealed stamp opened under the right key.
  \/ cs.keyOk
  \* The StampNotSealed defect: a stamp nobody sealed passes under any key.
  \/ Mutant = "StampNotSealed"

\* The newest log's stamp does not refuse the open: right key, and every
\* byte there for a sealed stamp. An earlier log was synced whole before the
\* next one existed, so only the newest log's stamp can be torn.
NewestStampOk(cs) ==
  \* The key half, as for every log.
  /\ StampKeyOk(cs)
  \* A torn sealed stamp cannot be told from one under another key, so it refuses like a wrong key.
  \* A torn checksummed stamp reads as a log nothing in which was synced, which replay drops, unless
  \* the planted bug UnstampedRefuses refuses it.
  /\ (~cs.stampTorn \/ (Frame = "Checksum" /\ Mutant # "UnstampedRefuses"))

\* The present records that are not usable.
Bad(cs) == {i \in 1..PLen(cs) : ~Usable(cs, i)}

\* O: the position of the first unusable record, or one past the end.
O(cs) == IF Bad(cs) = {} THEN PLen(cs) + 1 ELSE Min(Bad(cs))

\* P: how many leading records some usable record proves synced. Each
\* usable record's stamp counts; a usable CLOSE counts the whole file. In
\* offsets, record O starts below P exactly when O <= P here.
P(cs) ==
  \* The largest of: 0; the stamps of the usable records the file still holds;
  Max({0} \cup {log[i].st : i \in {j \in 1..PLen(cs) : Usable(cs, j)}}
          \* and the positions of usable CLOSE records (a CLOSE proves everything before it synced).
          \cup {i \in 1..PLen(cs) : Usable(cs, i) /\ log[i].c = 0})

\* Format 1's rule on the first damaged record: a partial record, or a
\* zero record followed only by zeros, ends the log; anything else refuses.
Format1Refuses(cs) ==
  \* There is a damaged record,
  /\ Bad(cs) # {}
  \* the first one is not a record the file simply ends inside,
  /\ cs.st[O(cs)] # "partial"
  \* and it is not a run of zeros to the end of the file.
  /\ ~(\A j \in O(cs)..PLen(cs) : cs.st[j] = "zero")

\* Replay refuses the newest log. The design refuses a stamp that fails,
\* and an unusable record below P. A record whose tag fails is unusable
\* like any damaged one, so in the tail above P it is dropped and
\* reported, and below P it refuses. Each mutant changes this rule as named.
RefusedNewest(cs) ==
  \* A stamp that fails refuses the open, naming the file.
  \/ ~NewestStampOk(cs)
  \* Then the O < P rule, or the defect that replaces it.
  \/ CASE Mutant = "DropBelowP"   -> FALSE
       \* The bug RefuseAboveP: any damaged record refuses.
       [] Mutant = "RefuseAboveP" -> Bad(cs) # {}
       \* The bug Format1: the old format's rule decides.
       [] Mutant = "Format1"      -> Format1Refuses(cs)
       \* The engine: a damaged record refuses when it lies at or below what a surviving stamp
       \* proves synced.
       [] OTHER                   -> Bad(cs) # {} /\ O(cs) <= P(cs)

\* Replay refuses an earlier log: they are complete, so any unusable
\* record there, or a stamp that fails, is damage.
RefusedSealed(cs) ==
  \* An earlier log garbled where no sync had covered it reads as damage...
  \/ cs.tear
  \* ...or some earlier log j...
  \/ \E j \in 1..Len(sealed) :
    \* ...whose stamp fails under this key...
    \/ ~StampKeyOk(cs)
    \* ...or that holds a record that is not usable: rot, or a failed tag.
    \/ \E i \in 1..Len(sealed[j]) : ~(sealed[j][i].ok /\ (Frame = "Checksum" \/ cs.keyOk))

\* Recovery refuses to open.
Refused(cs) == RefusedSealed(cs) \/ RefusedNewest(cs)

\* What recovery yields when it opens: the commits in tables, every earlier
\* log's commits, then the newest log's records before O.
Recovered(cs) == tables \o SealedCommits \o CommitsOf(SubSeq(log, 1, O(cs) - 1))

\* Recovery reports a discarded tail: it dropped bytes the file holds,
\* which is a warn line, two tickers and an EventListener callback.
Discarded(cs) == O(cs) <= PLen(cs)

----------------------------------------------------------------------------
\* Actions.

\* The start: no earlier log, an empty newest log whose stamp is synced.
Init ==
  \* No earlier log.
  /\ sealed  = <<>>
  \* The newest log is empty.
  /\ log     = <<>>
  \* Nothing in it is synced.
  /\ synced  = 0
  \* No CLOSE was appended.
  /\ closed  = FALSE
  \* No commit yet.
  /\ next    = 0
  \* No crash yet.
  /\ crashes = 0
  \* No table yet.
  /\ tables  = <<>>
  \* Nothing sealed in place.
  /\ inplace = FALSE
  \* The newest log exists.
  /\ haslog  = TRUE

\* A commit group appends its record, stamped with the completed sync.
AppendRecord ==
  \* The log is not closed.
  /\ ~closed
  \* Close did not seal the last memtable in place: no commit is decided after that.
  /\ ~inplace
  \* The model may still commit.
  /\ next < MaxCommits
  \* A record with the next commit number, stamped with the current synced count, joins the end of
  \* the log.
  /\ log'  = Append(log, [c |-> next + 1, st |-> synced])
  \* One more commit was appended.
  /\ next' = next + 1
  \* Nothing else changes.
  /\ UNCHANGED <<sealed, synced, closed, crashes, tables, inplace, haslog>>

\* A sync that began when k records were written completes: they are
\* durable. Records appended while it ran keep the older stamp.
Sync(k) ==
  \* The sync covers more than was synced, and no more than was written.
  /\ synced < k /\ k <= Len(log)
  \* Those k records are durable now.
  /\ synced' = k
  \* Nothing else changes.
  /\ UNCHANGED <<sealed, log, closed, next, crashes, tables, inplace, haslog>>

\* Rotation (E2, WalRotation.tla): the newest log is synced, sealed, and a
\* new log takes the next record. Also the way a close that failed after
\* sealing in place takes writes again (Ordered::reopen_log): the log it
\* left unsynced is synced before the next one exists.
Rotate ==
  \* The log is not closed.
  /\ ~closed
  \* It holds something.
  /\ log # <<>>
  \* One more file may be opened.
  /\ Len(sealed) + 1 < MaxFiles
  \* The newest log joins the earlier logs, whole, usable and durable: the rotation synced it.
  /\ sealed' = Append(sealed, [i \in 1..Len(log) |->
                 [c |-> log[i].c, st |-> log[i].st, ok |-> TRUE, dur |-> TRUE]])
  \* A new, empty log is the newest.
  /\ log'    = <<>>
  \* Nothing in it is synced.
  /\ synced' = 0
  \* A seal in place, if there was one, is over: commits go to the new log.
  /\ inplace' = FALSE
  \* Nothing else changes.
  /\ UNCHANGED <<closed, next, crashes, tables, haslog>>

\* A clean close appends CLOSE once every record is synced; a Sync then
\* makes CLOSE durable. The mutant appends it without the first sync.
\* Format 1 has no CLOSE.
AppendClose ==
  \* The log is not closed yet.
  /\ ~closed
  \* Close did not seal the last memtable in place (that close ends with CloseNext).
  /\ ~inplace
  \* Format 1 has no CLOSE record.
  /\ Mutant # "Format1"
  \* Every record is synced (the planted bug CloseWithoutSync skips this).
  /\ synced = Len(log) \/ Mutant = "CloseWithoutSync"
  \* A CLOSE record, stamped with the current synced count, joins the end of the log.
  /\ log'    = Append(log, [c |-> 0, st |-> synced])
  \* The log is closed.
  /\ closed' = TRUE
  \* Nothing else changes.
  /\ UNCHANGED <<sealed, synced, next, crashes, tables, inplace, haslog>>

\* A flush of the oldest frozen memtable: its commits join the tables, and
\* its log, whose writes the same synced manifest batch says are in a table,
\* is no longer replayed, and is removed.
Flush ==
  \* An earlier log is waiting for its memtable's flush.
  /\ sealed # <<>>
  \* Its commits are in a table now, durably.
  /\ tables' = tables \o CommitsOf(Head(sealed))
  \* Its log is skipped by recovery and removed.
  /\ sealed' = Tail(sealed)
  \* Nothing else changes.
  /\ UNCHANGED <<log, synced, closed, next, crashes, inplace, haslog>>

\* Close seals the last memtable in place (Ordered::seal_in_place): its log
\* stays the newest, unsynced, and takes no more records. The planted bug
\* NewLogBeforeFlush creates the next log right away instead, which makes
\* the unsynced log an earlier one before its flush is durable.
SealInPlace ==
  \* The log is not closed and not sealed in place already.
  /\ ~closed /\ ~inplace
  \* The memtable holds something: the log holds a commit.
  /\ \E i \in 1..Len(log) : log[i].c # 0
  \* Either the planted bug or the engine's step:
  /\ IF Mutant = "NewLogBeforeFlush"
       \* the bug: one more file may be opened,
       THEN /\ Len(sealed) + 1 < MaxFiles
            \* the unsynced log becomes an earlier log as it is, durable only where a sync covered it,
            /\ sealed' = Append(sealed, [i \in 1..Len(log) |->
                           [c |-> log[i].c, st |-> log[i].st, ok |-> TRUE, dur |-> i <= synced]])
            \* and a new, empty log is the newest, with nothing synced;
            /\ log' = <<>>
            \* nothing in it is synced,
            /\ synced' = 0
            \* and no seal in place is left to finish.
            /\ UNCHANGED inplace
       \* the engine: the log is sealed in place, nothing else moves.
       ELSE /\ inplace' = TRUE
            \* The log, its synced count and the earlier logs stay as they are.
            /\ UNCHANGED <<sealed, log, synced>>
  \* Nothing else changes.
  /\ UNCHANGED <<closed, next, crashes, tables, haslog>>

\* The flush of the memtable sealed in place, once every earlier one is in a
\* table (flushes go oldest first): its commits join the tables in one
\* synced manifest batch that also says recovery skips its log, and then
\* the log is removed. The pages it still had dirty are dropped, never
\* written. No log file exists until CloseNext.
FlushInPlace ==
  \* Close sealed the last memtable in place.
  /\ inplace
  \* Every earlier memtable is flushed already.
  /\ sealed = <<>>
  \* Its commits are in a table now, durably.
  /\ tables' = tables \o CommitsOf(log)
  \* Its log is gone.
  /\ log' = <<>>
  \* So nothing of it is synced.
  /\ synced' = 0
  \* No newest log file exists.
  /\ haslog' = FALSE
  \* The seal in place is done.
  /\ inplace' = FALSE
  \* The database takes no commit any more.
  /\ closed' = TRUE
  \* Nothing else changes.
  /\ UNCHANGED <<sealed, next, crashes>>

\* The end of a close that sealed in place (Ordered::close_log): the next
\* log is created and CLOSE appended to it, unsynced. A Sync then makes the
\* stamp and CLOSE durable together: the merged sync, since no record
\* precedes CLOSE.
CloseNext ==
  \* The in-place flush removed the log, and no next log exists yet.
  /\ ~haslog
  \* A next log exists now.
  /\ haslog' = TRUE
  \* It holds CLOSE, stamped with nothing synced.
  /\ log' = <<[c |-> 0, st |-> 0]>>
  \* Nothing else changes.
  /\ UNCHANGED <<sealed, synced, closed, next, crashes, tables, inplace>>

\* A crash (or a reopen after a clean close) and a recovery that opens.
\* The newest log is truncated at O, synced, and becomes an earlier log;
\* a new log takes the next commit, numbered after the last recovered one.
\* The mutant NoTruncate leaves the dropped tail in the file.
Crash ==
  \* One more crash is allowed.
  /\ crashes < MaxCrashes
  \* One more file may be opened.
  /\ Len(sealed) + 1 < MaxFiles
  \* The crash leaves the newest log in some crash state
  /\ \E cs \in CrashStates :
       \* from which recovery opens.
       /\ ~Refused(cs)
       \* The newest log, if a file exists, becomes an earlier log:
       /\ sealed' = IF ~haslog THEN sealed ELSE Append(sealed,
            \* under the bug NoTruncate,
            IF Mutant = "NoTruncate"
              \* every record the file still holds, usable or not;
              THEN [i \in 1..PLen(cs) |->
                      [c |-> log[i].c, st |-> log[i].st, ok |-> Usable(cs, i), dur |-> TRUE]]
              \* in the engine, only the records before the first unusable one: the rest was cut off
              \* and the cut was synced.
              ELSE [i \in 1..(O(cs) - 1) |-> [c |-> log[i].c, st |-> log[i].st, ok |-> TRUE, dur |-> TRUE]])
       \* Commits are numbered on from the last recovered one.
       /\ next' = Len(Recovered(cs))
  \* A new, empty log is the newest.
  /\ log'     = <<>>
  \* Nothing in it is synced.
  /\ synced'  = 0
  \* It is not closed.
  /\ closed'  = FALSE
  \* One more crash happened.
  /\ crashes' = crashes + 1
  \* The open creates the new log, and no seal in place is under way.
  /\ haslog'  = TRUE
  \* A crash ends any close that was under way.
  /\ inplace' = FALSE
  \* What tables hold stays.
  /\ UNCHANGED tables

\* Every step the system can take.
Next ==
  \* A commit group appends its record.
  \/ AppendRecord
  \* A sync completes.
  \/ \E k \in 1..(MaxCommits + 1) : Sync(k)
  \* The log is rotated.
  \/ Rotate
  \* A clean close appends CLOSE.
  \/ AppendClose
  \* The oldest frozen memtable is flushed.
  \/ Flush
  \* Close seals the last memtable in place.
  \/ SealInPlace
  \* Its flush.
  \/ FlushInPlace
  \* The next log, created and closed.
  \/ CloseNext
  \* The machine crashes and recovers.
  \/ Crash

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants. Each quantifies over every crash state at this moment, so a
\* crash at every step of every behaviour is checked.

\* Every variable holds what its comment says.
TypeOK ==
  \* Fewer earlier logs than the file bound.
  /\ Len(sealed) < MaxFiles
  \* The synced count never exceeds what was written.
  /\ synced \in 0..Len(log)
  \* The closed flag is a yes or a no.
  /\ closed \in BOOLEAN
  \* The commit count stays within the bound.
  /\ next \in 0..MaxCommits
  \* No more crashes than the budget.
  /\ crashes \in 0..MaxCrashes
  \* The tables hold no more commits than were made.
  /\ Len(tables) <= MaxCommits
  \* The two flags are yes or no.
  /\ inplace \in BOOLEAN /\ haslog \in BOOLEAN
  \* No log file means no record and nothing synced.
  /\ (~haslog => log = <<>>)

\* A crash the device's flush honoured, opened with the right key, never
\* leaves a database that refuses to open. It rules out, say, a power cut
\* just after a rotation created log 2: reopened with the right key, the
\* database must open, whatever state log 2's records are in. Lean:
\* recovers_prefix2, and sealed_stamp_opens_after_honest_crash with its
\* RED unsynced_stamp_refuses_honest_crash.
RecoveryOpens ==
  \* Every crash state...
  \A cs \in CrashStates :
    \* ...reopened with the right key, with no rot in the synced prefix...
    cs.keyOk /\ ~cs.rot
      \* ...opens.
      => ~Refused(cs)

\* Whatever recovery yields when it opens is commits 1..n: the Eventual
\* promise, and true in every mode. Lean: recovers_prefix2.
\* Rules out, for example: after a power cut the database opens holding commits 1 and 3 but not 2.
NoGap == \A cs \in CrashStates : ~Refused(cs) => IsPrefix(Recovered(cs))

\* Every commit a completed sync covered survives a crash the flush
\* honoured. Lean: recovers_prefix2, its bound.
KeepsSynced ==
  \* For every crash state that opens and has no rot: every commit a sync covered is among the
  \* recovered ones. Rules out a commit that reached the disk and is missing after the restart.
  \A cs \in CrashStates : ~Refused(cs) /\ ~cs.rot => SyncedCommits \subseteq Range(Recovered(cs))

\* THE IMMEDIATE PROMISE. Every acknowledged commit survives any crash the
\* device's flush honours. Lean: recovers_prefix2, since Immediate
\* acknowledges exactly the commits a completed sync covered.
AckedSurvive ==
  \* Under Immediate:
  Mode = "Immediate" =>
    \* for every crash state that opens and has no rot, every acknowledged commit is among the
    \* recovered ones. Rules out, for example: a caller is told "committed", the power is cut, and
    \* after the restart the commit is gone.
    \A cs \in CrashStates : ~Refused(cs) /\ ~cs.rot => Acked \subseteq Range(Recovered(cs))

\* Replay never discards a record a surviving stamp proves synced: below P
\* it refuses instead. Lean: replay2_keeps_proven.
NoProvenLoss ==
  \* For every crash state that opens:
  \A cs \in CrashStates : ~Refused(cs) =>
    \* every commit among the first P records (those a surviving stamp proves synced) is recovered.
    \* Rules out, for example: record 2 is damaged, a later record's stamp says "the first 3 records
    \* were synced", and replay quietly drops records 2 and 3 as a torn tail.
    \A i \in 1..P(cs) : log[i].c # 0 => log[i].c \in Range(Recovered(cs))

\* A recovery that opens without a synced commit has reported a discarded
\* tail: residual damage is never silent. Lean: residual_is_reported.
LossIsReported ==
  \* For every crash state
  \A cs \in CrashStates :
    \* that opens without some synced commit: a discarded tail was reported. Rules out a database
    \* that lost durable data and said nothing.
    ~Refused(cs) /\ ~(SyncedCommits \subseteq Range(Recovered(cs))) => Discarded(cs)

\* An open under the wrong key refuses; it never drops the log as a tail.
\* It rules out a provider that hands back other bytes under the right key
\* id and a database that opens empty because every tag failed. Lean:
\* wrong_key_refuses and wrong_key_refuses_sealed, and
\* unsealed_stamp_drops_log for the RED.
WrongKeyRefuses ==
  \* Every crash state reopened under the wrong key...
  \A cs \in CrashStates : ~cs.keyOk
    \* ...refuses to open.
    => Refused(cs)

====
