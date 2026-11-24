-- The root of the Regolith proof library: importing it builds every module.
-- Each module's header names the TLA+ model and invariant it backs.

-- E1: the read order returns the newest visible version, and compacting a
-- closed L0 input set keeps that. Backs `proofs/tla/LsmOrder.tla`.
import Regolith.LsmOrder
-- E14: a range delete copied into the first layer a read walks deletes
-- every older key below it, however the layers are ordered. Backs
-- `proofs/tla/LegacyRangeRead.tla`.
import Regolith.RangeDelete
-- E6: a batch read resolved against one view answers as of one point in
-- time. Backs `proofs/tla/BatchRead.tla`.
import Regolith.BatchView
-- E8: a visible sequence that never passes a pending slot gives repeatable
-- snapshots. Backs `proofs/tla/IngestPublication.tla`.
import Regolith.Publication
-- E2: syncing the sealed log before the next log takes a record makes
-- recovery a gap-free prefix. Backs `proofs/tla/WalRotation.tla`.
import Regolith.WalRecovery
-- E46: a close that seals its last log in place and creates the next one
-- only after the flush is durable, and a close with no record that syncs
-- once, keep recovery a gap-free prefix. Backs `proofs/tla/WalRecovery.tla`.
import Regolith.WalClose
-- Plan 3.6: commit-ordered append numbers appends densely from 1, once per
-- once key, in commit order, and a snapshot sees exactly rows 1..H. Backs
-- `proofs/tla/CommitOrderedAppend.tla` and `CommitOrderedAppendCrash.tla`.
import Regolith.Append
-- Dropping a range of a commit-ordered log inside a transaction keeps the log's
-- law (live entries are the positions to the head less the truncated ones), a
-- pinned reader's view, and never conflicts. Backs `proofs/tla/LogTruncate.tla`.
import Regolith.LogTruncate
-- A transaction reads its own appends to a log, oldest first, the order the commit numbers them.
-- Backs `proofs/tla/AppendedEntries.tla`.
import Regolith.Appended
-- Plan 3.7: conflict-free allocation reserves ranges that never overlap,
-- only grow, and survive crashes. Backs `proofs/tla/Allocate.tla`.
import Regolith.Allocate
-- 4.8, E5: reducing a key's versions per snapshot stripe, with an exact
-- partial_merge, changes no read at a live snapshot or at the head. Backs
-- `proofs/tla/StripeCompaction.tla`.
import Regolith.Stripes
-- 4.6: per-thread slots with counted entries (announce, sample, confirm;
-- copies join, releases go where the pin was recorded) keep the
-- compaction's minimum at or below every live snapshot and every count
-- exact. Backs `proofs/tla/SnapshotRegistry.tla`.
import Regolith.SnapshotRegistry
-- 4.6: per-thread statistics shards sum to every increment, and a read
-- concurrent with adds is bounded and never goes backwards. Backs
-- `proofs/tla/ShardedStats.tla`.
import Regolith.ShardedStats
-- WalRecovery.lean also holds format 2 replay (4.2), backing
-- `proofs/tla/WalRecovery.tla`; LsmOrder.lean also holds flush order,
-- ingest placement, overlap demotion (E14) and binary search.
-- 4.7: deciding in ring order gives dense sequences in ring order, a
-- reader at the published horizon sees exactly the published commits, a
-- rollback leaves the sequences as if the discarded slots had never been
-- decided, and a sync vouches only for what is written. Backs
-- `proofs/tla/CommitPipeline.tla`.
import Regolith.Pipeline
-- 4.7: every ordering rule of the pipeline looks back only, so one thread
-- alone finishes every slot up to its own, wherever the others stopped.
-- Backs `proofs/tla/CommitPipeline.tla`.
import Regolith.PipelineHelping
-- E10, 4.7: the check a slot runs when it is decided. A write that landed
-- is in the store every later slot is checked against, so no update is
-- lost; an abort has a cause; and the check split at a horizon decides as
-- one check. Backs `proofs/tla/CommitPipeline.tla`.
import Regolith.PipelineCheck
-- Ordered progress through a cold early validation.
import Regolith.ValidationProgress
-- 4.7: a commit's landing listeners. Whoever waits for a commit to land is
-- told at most once, and a closed list holds nobody. Backs
-- `proofs/tla/Landing.tla`.
import Regolith.Landing
-- 4.10: a queue's poll never waits for another thread. A job that parks for
-- something another thread holds leaves a note and looks again, so it is
-- told when that thing is given up. Backs `proofs/tla/Parking.tla`.
import Regolith.Parking
-- 4.6: the version store's step. One thread at a time runs the sections
-- handed over, in the order handed over, and none is left behind. Backs
-- `proofs/tla/VersionStep.tla`.
import Regolith.VersionStep
-- Plan 3.3: an exact partial_merge folds to full_merge in any grouping, and
-- the touches law. Backs `proofs/tla/RepeatableRead.tla`.
import Regolith.MergeOperator
-- Same-commit partial merges keep reads and leave earlier commits untouched.
import Regolith.BatchMergeFold
-- Plan 3.1 to 3.15: every key class's commit rule accepts only histories
-- equal to the serial one in commit order, up to the class's relaxation.
-- Backs `proofs/tla/RepeatableRead.tla`.
import Regolith.Validation
-- Each key class's relaxation stated exactly, and the RED counterexamples.
-- Backs `proofs/tla/RepeatableRead.tla`.
import Regolith.Relaxations
-- regolith::sync's locks after D49: barging with a bypass bound gives
-- mutual exclusion, at most BOUND lost races per waiter, an exclusive owed
-- handoff and no stranded head. Backs `proofs/tla/Sync.tla`, the write
-- side of `SyncRwLock.tla` and the turn semaphore of `SyncOnce.tla`.
import Regolith.Sync
-- regolith::sync::Notify: the FIFO waiter queue with handoff before wake
-- gives FIFO service and no lost wakeup, cancellation included. Backs
-- `proofs/tla/SyncNotify.tla` and the waiter list of `SyncLatch.tla`.
import Regolith.SyncFifo
-- 3.16: every transaction callback runs exactly once, of its attempt's
-- outcome, in the stated order, and attempts are isolated. Backs
-- `proofs/tla/TxnCallbacks.tla`.
import Regolith.Callbacks
-- E29, E30: the open accepts every crash state of the manifest, keeps every
-- synced batch, and replays no version above a newer table. Backs
-- `proofs/tla/ManifestRecovery.tla`.
import Regolith.ManifestRecovery
-- D48: an ingest that survives a crash keeps every commit ordered before it.
-- Backs `proofs/tla/IngestDurability.tla`.
import Regolith.IngestDurability
-- E27: retiring a tombstone no deeper run meets and no live snapshot is below
-- changes no snapshot's read. Backs `proofs/tla/TombstoneRetirement.tla`.
import Regolith.TombstoneRetirement
-- 4.10, D53: per-thread I/O queues. A unit is claimed by one CAS and runs
-- once, every registered queue is told exactly once and no other is, a
-- request sits on its own queue, an idle owner is woken and a busy one
-- never, and one thread finishes everything. Backs
-- `proofs/tla/NonBlocking.tla`.
import Regolith.IoQueue
-- 4.12, D45: a sealed manifest batch keeps its checksum, checked before any
-- key, so a torn batch and a wrong or missing key are never confused.
-- Backs `proofs/tla/ManifestSeal.tla`. (The sealed log stamp is in
-- WalRecovery.lean.)
import Regolith.ManifestSeal
-- 4.6, E28: the compaction gate. A job is never inside with a pass or with
-- another job, no waiter is passed over more than BOUND times (so jobs
-- cannot starve the worker's pass, nor passes a job), the worker runs a
-- pass between any two jobs, and a job parked on a queue always keeps a
-- way to be woken. Backs `proofs/tla/CompactionGate.tla`.
import Regolith.CompactionGate
-- 4.12, D57: a backup of an encrypted database seals its metadata; a
-- restore checks every key before its first write, copies exactly what the
-- tag covers and writes a sealed MANIFEST last. Backs
-- `proofs/tla/BackupSeal.tla`.
import Regolith.BackupSeal
-- 4.6, Phase 7b: the wait-free read view. A reader never holds a freed
-- view nor one older than what was published when its load began, and the
-- compare-and-swap publication loses no publication. Backs
-- `proofs/tla/ReadView.tla`.
import Regolith.ReadView
-- 4.6, Phase 7 integration: every publisher of the read view (a rotation, a
-- flush's install and retire on every flush path, a compaction, an ingest)
-- publishes by compare-and-swap, so no memtable or table is lost and no
-- retired memtable comes back. Backs `proofs/tla/ReadViewPublishers.tla`.
import Regolith.ReadViewPublishers
-- 4.6, Phase 7b: the lock-free CLOCK block cache. The hand evicts only a
-- block nobody holds and no landing names, counts stay exact, and the
-- two-level reservation keeps the byte bound. Backs
-- `proofs/tla/ClockCache.tla`.
import Regolith.ClockCache
-- 4.6, Phase 7b: a memtable's append-only range-tombstone log. Readers see
-- a whole prefix of the appends, including every one a snapshot they took
-- includes. Backs `proofs/tla/TombstoneLog.tla`.
import Regolith.TombstoneLog
-- 4.6, Phase 7c1: the open-file slot table under max_open_files. Never
-- more open files than slots, never a file closed under a reader, and a
-- read returns its own table. Backs `proofs/tla/OpenFileTable.tla`.
import Regolith.OpenFileTable
-- D60: a queue unit's reopen under max_open_files never waits for a slot,
-- and a parked one is never forgotten: its message is there, or a WANTED
-- busy slot or a wake on its way will bring it. The model and NeverWaits;
-- the invariant proof of RetryNeverLost; the four RED counterexamples.
-- Backs the D60 rows of `proofs/tla/OpenFileTable.tla`.
import Regolith.OpenFileNoWait
-- D60, second half: every step keeps the facts, so a parked unit is never lost
-- (`retry_never_lost`).
import Regolith.OpenFileNoWaitProof
-- D60, the RED half: each planted bug breaks the rule it is planted against.
import Regolith.OpenFileNoWaitRed
-- 4.6, Phase 7c1: column families created and dropped in the ordered step.
-- No write lands after its family's tombstone or before its birth, and a
-- family's life only moves forward. Backs `proofs/tla/CfRegistry.tla`.
import Regolith.CfRegistry
-- 4.6, Phase 7c1: the env file maps. A positional read gets its own
-- offset's bytes, and an in-memory read sees every append that finished
-- before it began. Backs `proofs/tla/EnvFiles.tla`.
import Regolith.EnvFiles
-- A table renamed aside while its handles reopen it: the Env's rename adds
-- the new name before it drops the old, and a reopen that found nothing
-- loads the names again, so it always finds the file. Backs Part 3 of
-- `proofs/tla/EnvFiles.tla`.
import Regolith.EnvFilesRename
-- Phase 7 integration: a foreground job that held the compaction gate,
-- with no worker and inline compaction on, relieves the stop it kept
-- stopped writers from relieving, so no writer waits on a stall nobody will
-- land. E35: an engine that latches lands the stall's unit, and a rotation
-- counts its progress whether its seal went through or failed, so no writer
-- waits on a latched engine. E36: recording the failure of the work that
-- relieves a stop lands the unit too, so a writer that was waiting is told
-- what a fresh write is told. Backs `proofs/tla/StallRelief.tla`.
import Regolith.StallRelief
-- A commit is never lost to a stall: its call ends with its write in or
-- told a failure, never handed the wait a plain write is (inline compaction
-- off included) and never given up while it makes room. Backs the commit
-- rows of `proofs/tla/StallRelief.tla`.
import Regolith.StallCommit
-- E15: waiting for the last snapshot. A sleeping waiter always has a pin
-- still held or a release on its way to wake it, and on a target with one
-- thread the call returns the count of pins held without waiting. Backs
-- `proofs/tla/SnapshotWait.tla`.
import Regolith.SnapshotWait
-- E16: with no worker, a write that runs its owed step inline runs exactly
-- the one step of background work it found owed, and L0 never passes its
-- compaction trigger (its stop trigger when every write seals); with the
-- step left on the writer's queue, L0 never passes the stop trigger plus
-- the sealed memtables a rotation keeps. Backs `proofs/tla/OwedStep.tla`.
import Regolith.OwedStep
-- 4.6, 4.10: the pessimistic lock table. One holder per key, a retired
-- table entry has no user, a waiter the lock may serve is told through its
-- own inbox and nowhere else, a waiter loses at most BOUND races, and a
-- commit keeps its lock until it is visible. Backs
-- `proofs/tla/LockTable.tla`.
import Regolith.LockTable
-- The slots close finds the open transactions' claims in: `place` maps
-- every slot below the capacity to exactly one chunk cell and back, every
-- slot plus one fits the free stack's 32-bit top, and the stack's change
-- count makes a word read before 1 to 2^32 - 1 changes differ from the word
-- after them. Backs `proofs/tla/ClaimSlots.tla`.
import Regolith.ClaimSlots
-- An in-memory file's extent: a byte below the room lies in exactly one
-- segment, the walk cuts a range into pieces inside their segments with no
-- gap or overlap, and a link moves no byte and at least doubles the room.
-- Backs `proofs/tla/MemFileExtents.tla`.
import Regolith.MemFileExtents
-- A key's versions held first-in-place read exactly as the list they
-- replaced, and the merge read's fast path answers what the full path does.
-- No TLA+ model: a chain is one read's local value, never shared.
import Regolith.MergeChain
-- A histogram sample skips only updates that change nothing (the minimum
-- only falls, the maximum only rises, adding 0 is nothing), so the skips
-- lose no sample. Backs `proofs/tla/HistogramBounds.tla`.
import Regolith.HistogramBounds
-- E20 and #289: OPFS mirror mode's persist. With the lock taken before a
-- batch captures, the MANIFEST version OPFS holds never goes down; a failed
-- removal stays owed and a NotFoundError counts as removed; and a batch in
-- persist order cut short anywhere has written every table before the
-- MANIFEST and every file before a removal. Backs
-- `proofs/tla/MirrorPersist.tla`.
import Regolith.MirrorPersist
-- Completed I/O stays retained for a live caller despite intervening polls.
import Regolith.LandingLifetime
-- A let-go landing frees its block whatever kovan's reclamation schedule:
-- `proofs/tla/LandingReclaim.tla`.
import Regolith.LandingReclaim
