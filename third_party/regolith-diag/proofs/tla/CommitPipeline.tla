---- MODULE CommitPipeline ----
\* The lock-free commit pipeline as built (plan 4.7; src/engine/commit).
\*
\* THE PICTURE. Think of a bakery with a ticket machine. Every customer (a
\* commit) pulls a numbered ticket (a slot of the ring). Orders are handled
\* strictly in ticket order, but by whoever is standing at the counter: any
\* customer may do the next step of any order ahead of their own. So a
\* customer who wanders off never holds the queue up: the next one finishes
\* their order for them, or throws their unused ticket away.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/Pipeline.lean:
\*   seqs_dense, seqs_strictly_increasing
\*       NoHole, SeqInRingOrder.
\*   publication_order, reader_sees_published
\*       PublishedPrefix, ReadersSeePrefix.
\*   rollback_as_if_never_decided
\*       NoHole across a rollback: the sequences are the ones the kept and
\*       the later slots would have taken had the discarded ones never
\*       been decided.
\*   rollback_keeps_published, rollback_finishes_everything
\*       a rollback touches nothing published and leaves nothing waiting
\*       (G2, PublishedPrefix).
\*   sync_covers_only_written
\*       DurableImpliesWritten.
\* In proofs/lean/Regolith/PipelineHelping.lean:
\*   first_unfinished_can_step, helper_alone_finishes, no_step_goes_back,
\*   helping_is_bounded
\*       the safety half of lock-freedom: every ordering rule looks only at
\*       earlier slots, so the first unfinished slot always has a step any
\*       thread at or behind it may take; no step of any thread undoes
\*       another's; so one live thread alone finishes every slot up to its
\*       own in a bounded number of its own steps (E21), wherever the
\*       others stopped.
\*   held_round_blocks
\*       the stated limit: a thread that stops holding the round stops the
\*       commits that wait for the sync (RED StopHoldsClaim).
\* And in Regolith/PipelineCheck.lean, the check a slot runs when it is
\* decided:
\*   landed_write_is_seen, no_lost_update, later_slot_aborts
\*       a write that landed is in the store every later slot is checked
\*       against, since a slot is in the memtable before the next is
\*       decided; so a later transaction that touched its key from an older
\*       snapshot aborts, whatever slots stand between them (NoLostUpdate).
\*   abort_has_cause
\*       a slot aborts only when it is a transaction and the store holds a
\*       newer version of a key it touched (AbortsHaveCause).
\*   conflict_stays
\*       a conflict found among some versions is still one when more land,
\*       which is why an early conflict aborts at once (EarlyAbort).
\*   early_check_split, early_check_cover
\*       checking up to a horizon on the committer's own thread and above
\*       it when the slot is decided equals one check (NoLostUpdate,
\*       AbortsHaveCause).
\*   store_below_ceiling, ceiling_skips_the_check_above
\*       no version is newer than the last sequence handed out, so when
\*       that is at or below the horizon the look above it finds nothing
\*       and is skipped (SkipsCheck, Loses).
\*   published_only_loses_an_update, skip_on_published_loses_an_update
\*       RED ValidatePublished and RED SkipOnPublished as concrete
\*       counterexamples.
\* The other half of lock-freedom, that the interleavings of several
\* threads keep finishing slots (LockFree, EveryLiveCommitCompletes), is
\* checked here by TLC, on small instances.
\*
\* THE CODE, step by step, and the action that stands for each step.
\*   Begin, EarlyAbort
\*               Transaction::begin, and RegolithEngine::check_early
\*               (early.rs): before it takes a slot a transaction samples
\*               the read horizon and checks itself, on its own thread,
\*               against everything visible up to it; a conflict there
\*               aborts it at once. Whichever thread decides its slot then
\*               looks only above that horizon (Conflict), and not at all
\*               when nothing was handed a sequence above it (SkipsCheck).
\*   Reserve     Ring::reserve, one fetch_add on the tail.
\*   Fill        Ring::install, one compare-and-swap of the cell's stamp.
\*   Void        Ring::void by a helper that reaches a reserved cell with
\*               nothing in it; the owner's install then fails (Retry).
\*   ReadTip, DecideCas
\*               RegolithEngine::decide: read the tip (the state after the
\*               newest decided slot), check the slot is next, compute the
\*               decision, install it with one compare-and-swap
\*               (Descriptor::decide). An abort takes nothing. "Abort" here
\*               is every decision that lands nothing: a lost conflict, a
\*               dropped column family (the fence), a database that is
\*               closed, a validation read that would block (R11).
\*   Apply, TipMove, MarkStale, Pass
\*               RegolithEngine::settle: apply the slot to its memtable,
\*               move the tip to its decision, pass the decided frontier. A
\*               decision whose epoch is not the tip's is stale: its slot is
\*               failed instead.
\*   Write, WriteFail
\*               the record reaches the log in one step, by whichever
\*               thread may write it, behind the log's epoch gate: the rows
\*               that leave Copies unset show the write no finer than that.
\*               A failed write poisons the log.
\*   Encode, Gather, RunStart, WriteLand, Release, Park, Timeout,
\*   StealStart, WriteStart, LandFail
\*               the write as the code makes it, in the rows that set
\*               Copies (write_unit.rs, the write unit; the table at the
\*               end of this header names the line of code for each).
\*               Each owner encodes its own record (Encode). One thread at a
\*               time holds the write unit: it gathers the run of filled
\*               records that waits first (Gather), counts in at the gate
\*               and looks at them again (RunStart), writes them with one
\*               write that lands (WriteLand) or fails (LandFail), and lets
\*               the unit go, waking the first thread still waiting for it
\*               (Release). A thread whose record is in another's hands
\*               parks on it (Park), and is woken by whoever writes, fills
\*               or fails it; or its patience runs out (Timeout) and it
\*               writes the record itself, as a copy, counted in at the
\*               gate and looked at again first (StealStart). The round's
\*               holder copies what it needs without waiting (WriteStart).
\*               A sync waits for the writes in flight below what it
\*               vouches for before it begins, and so does a unit before
\*               it syncs, since a sector in flight when the power goes may
\*               come back garbled whatever was being written to it
\*               (wal_gate.rs, WG4; NoWriteUnderSync).
\*   ClaimRound, SyncBegin, SyncOk, SyncFail
\*               round.rs: the round is one claim on the log's device step.
\*               Whoever claims it syncs the written prefix.
\*   RbFreeze, RbFinish
\*               ordered.rs roll_back, run by the thread that holds the
\*               round when the log is poisoned: freeze the frontiers, move
\*               to a new epoch, publish what is publishable, fail the rest,
\*               cut the log, replace the memtable, thaw.
\*   PubHorizon, PubFrontier
\*               publish_upto: raise the read horizon, then pass the
\*               published frontier.
\*   ClaimNext, UnitRun, TurnRun
\*               ordered.rs: an ordered unit (an ingest's install, a column
\*               family's create or drop, a close) takes a slot, and the
\*               rotation check takes the turn of the write that finds the
\*               memtable full. Either is a job claimed with one
\*               compare-and-swap by whichever thread reaches the slot, and
\*               runs with everything before it published.
\*
\* WHAT IS NOT A STOPPED THREAD. A thread that holds the round, or that
\* claimed an ordered unit, is taken to be at work on the device (an fsync,
\* a truncate, a flush, a new log file), and the model never stops it
\* there. This is a limit of the code and not only of the model: between
\* its claim and its end such a thread also runs ordinary code, and a
\* thread descheduled for good in that window holds up every commit that
\* needs the round or stands behind the unit. The RED StopHoldsClaim shows
\* exactly that. Nor does the model stop a thread whose write is in flight
\* (counted in at the gate): those bytes are with the host, which finishes
\* the write. Everywhere else a thread may stop for good, and that
\* includes the holder of the write unit with a run gathered: the others'
\* patience runs out and they write their records themselves
\* (MC_CommitPipeline_Green_Copies_Stop).
\*
\* SMALL STEPS AND ONE-GO STEPS. Deciding an order is four steps in the
\* code (install the verdict, apply, move the tip, pass the frontier) and
\* publishing it is two (horizon, frontier). Whatever another thread does
\* between two of those steps it could as well have done before the first
\* or after the last, and come out the same, with one exception: a
\* rollback, which may fall between any two of them. So a configuration may
\* set Coarse: the steps are then taken one by one for as long as a rollback
\* can still happen (the log may still fail, or a failure is being dealt
\* with), and in one go otherwise. With Coarse unset every step is always
\* taken on its own. The run that checks every invariant for three threads
\* and the runs of two threads leave it unset; the other runs of three set
\* it, which is what lets them finish in seconds. Each .cfg says which it
\* is, and what it gave up.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - One sequence per commit, and a record's offset is its sequence: each
\*     decided commit takes one contiguous range of each, in the same order,
\*     so one number stands for both.
\*   - The ring's capacity. A reservation that finds its cell in use helps
\*     publish the slot a lap before, the same helping as here; indices here
\*     only grow, up to MaxSlots, and BoundNeverBinds checks the bound never
\*     stops a live thread.
\*   - The pins that keep a loaded descriptor alive, the parking on a job,
\*     and the two loads a decider makes around a rotation: loom models in
\*     src/engine/commit/loom_model.rs.
\*   - commit_nowait. Its commits take the same steps; where a blocking
\*     call waits for a round or a unit another thread claimed, such a
\*     commit leaves the job on its own queue and returns. The queue and
\*     the delivery of the completion are NonBlocking.tla.
\*   - Two threads writing one verdict at once. Both work it out from the
\*     same tip, so both write the same one, and one compare-and-swap
\*     keeps one; the model writes it once. The exception is a thread
\*     whose tip a rollback has since discarded, and that one the model
\*     has (ReadTip, DecideCas).
\*   - Recovery: WalRecovery.tla and Regolith/WalRecovery.lean.
\*
\* CONFIGURATIONS are listed with what each shows in the .cfg files.
\*
\* WHAT BECAME OF GroupCommit.tla. The pipeline replaced group commit (one
\* leader that held a mutex, took the waiting commits as a group, and handed
\* the mutex on). That model is retired with the code it described. Every
\* rule it checked is carried by a rule here, or in the model named, and
\* every RED row it had has a RED row that plants the same mistake in the
\* pipeline:
\*
\*   SerialEquivalent           -> NoLostUpdate and AbortsHaveCause.
\*     RED ViewOnly             -> Red_ValidatePublished (a slot checked
\*                                 against a view that lacks an earlier
\*                                 slot's writes), Red_PassBeforeApply_LostUpdate
\*     RED TrustEarly           -> Red_TrustEarly
\*     (new with the skip)      -> Red_SkipOnPublished: the skip asked of
\*                                 the published horizon, not the last
\*                                 sequence handed out
\*   SeqFollowsDecisionOrder    -> SeqInRingOrder; Red_DecideAnyOrder_Order
\*   StoreHoldsCommits          -> AppliedBeforePassed and MemtableOnlyLive;
\*                                 Red_PassBeforeApply, Red_KeepMemtable
\*   VisibleIsApplied           -> ReadersSeePrefix; Red_OutOfOrder
\*   DurableBeforeVisible       -> DurableBeforeVisible, the same rule.
\*     RED PublishBeforeSync    -> Red_PublishBeforeSync
\*   TicketsCompleteOnce        -> OwnerSeesItsFate and NoFalseAck;
\*                                 Red_VoidNotCas, Red_RollbackForgets_Ack.
\*                                 For a commit made by commit_nowait:
\*                                 Landing.tla, ToldAtMostOnce;
\*                                 MC_Landing_Red_ReadThenClear
\*   TicketMeansVisible         -> PublishedIsVisible and NoFalseAck;
\*                                 Red_FrontierFirst
\*   BoundedTurn (E21)          -> WorksUpToItsOwn.
\*     RED DrainAll             -> Red_WorkBeyond
\*   NoStuckTicket              -> NoLiveThreadBlocked; Red_NoHelping,
\*                                 Red_StopHoldsClaim
\*   SingleSync                 -> OneRound and DurableWithinDecided.
\*     RED DoubleClaim          -> Red_TwoSyncs, Red_TwoSyncs_Durable; for
\*                                 the claim of a job, NonBlocking.tla,
\*                                 SingleRun, MC_NonBlocking_Red_DoubleSync
\*   OwedBlocksNextGroup        -> no longer a rule: commits behind a sync
\*                                 that is owed are decided and written
\*                                 meanwhile. What it protected, that nothing
\*                                 is published past a commit still waiting
\*                                 for its sync, is ReadersSeePrefix and
\*                                 DurableBeforeVisible
\*   AllTicketsComplete         -> LockFree and EveryLiveCommitCompletes, in
\*                                 every row whose name has Live in it and
\*                                 in the Rollback, Rotation, Early,
\*                                 ReadOnly and Plain rows
\*   NothingOwed (the witness   -> Parking.tla, NeverParked;
\*     Green_Nowait_Owes)          MC_Parking_Green_Parks
\*
\*   Green                      -> MC_CommitPipeline_Green
\*   Green_Live                 -> MC_CommitPipeline_Green_Live, and
\*                                 Green_Rollback for a group that fails
\*   Green_LivePlain            -> MC_CommitPipeline_Green_Plain
\*   Green_ReadOnly             -> MC_CommitPipeline_Green_ReadOnly
\*   Green_Nowait, _NowaitLive  -> MC_NonBlocking_Green_Commit (the queue
\*                                 and the delivery), MC_Parking_Green (a
\*                                 poll that cannot go on) and
\*                                 MC_Landing_Green (who is told)
\*
\* The group's own device, checking a member against the view plus the keys
\* of the members accepted before it, is gone with the group: a slot is in
\* the memtable before the next one is decided (AppliedBeforePassed), so the
\* check reads the memtable itself.
\*
\* THE WRITE UNIT, LINE BY LINE. Every variable, step and promise of this
\* model about the write unit and about a thread parked far from its turn,
\* beside the Rust that does it, in plain words. (File and line of the tree
\* the model was checked against; the files are under src/engine.)
\*
\* What the codes mean, in a line each: WU1 to WU8 are the write unit's
\* promises (write_unit.rs): one holder at a time; owners fill their own
\* record; a run looks at its records again once counted in; no lost wake;
\* a park is bounded; a run is bounded; a woken owner finds its commit
\* published; a writer that finds the unit free skips it. WG1 and WG4 are
\* the log gate's (wal_gate.rs): a discarded epoch writes nothing, and no
\* write lands under a sync that covers it. D4 and D5 are the slot's
\* (descriptor.rs): one encoder per record, and a park registers before it
\* looks again. D53 is a commit_nowait commit left on its queue. P5 and P6
\* are the pipeline's (pipeline.rs): helping is bounded, and a call far
\* from its turn sleeps until its turn nears; T1 to T3 are turn_wait.rs's.
\*
\*   filled[i]: order i's record is encoded and will not change again. The
\*       FILLED flag, set by Descriptor::fill (commit/descriptor.rs:593), read by
\*       Descriptor::record (commit/descriptor.rs:611).
\*   filling[i], encoding[a]: somebody claimed the encoding of order i's
\*       record, and which record thread a has in hand. The FILLING flag
\*       (commit/descriptor.rs:357), claimed by the fetch_or at the top of
\*       Descriptor::fill; the encoding is the closure that call runs.
\*   writer: who holds the write unit. The CLAIMED bit of WriteUnit.word,
\*       taken by try_claim (commit/write_unit.rs:219) and let go by release
\*       (commit/write_unit.rs:239).
\*   gathered[a]: the run the holder collected. The Run that try_batch's scan
\*       fills (commit/write_unit.rs:874), record by record through Run::take
\*       (commit/write_unit.rs:462).
\*   fly[a]: a write counted in at the gate, its bytes on the way. InFlight
\*       (wal_log.rs:641), made by Log::count_in (wal_log.rs:417) and counted out
\*       when it drops (wal_log.rs:659).
\*   runEnd[a]: the last record of the run thread a wrote. wrote_upto in
\*       try_batch (commit/write_unit.rs:967).
\*   asleep, sleepOn[a]: a thread parked on a record. The thread on the
\*       descriptor's list with PARKED set: Descriptor::register
\*       (commit/descriptor.rs:658), then park (commit/descriptor.rs:641).
\*   looking[a], saw[a]: a thread about to park, and the claim it saw when it
\*       found the unit taken (the `seen` of bring_written,
\*       (commit/write_unit.rs:853 return Tried::Claimed(word)>)). claimNo counts
\*       the claims, as the claim number in WriteUnit.word does; noted holds every
\*       claim a note ever landed on, which only the broken NoteTrustsALoad reads.
\*   late[a]: a parked thread whose patience ran out. out_of_patience answered
\*       yes (commit/write_unit.rs:675); copy_record comes next.
\*   wanted: a parked thread noted the current claim. The WANTED bit, put on
\*       by WriteUnit::note (commit/write_unit.rs:251) with a compare-and-swap
\*       that lands only while the claim it saw still holds the unit, and read by
\*       the let go's swap.
\*   turnAsleep, turnAt[a]: threads parked far from their turn, and where the
\*       frontier stood when each last parked. Turns.far counts them
\*       (commit/turn_wait.rs:52); parked_at in help_decide
\*       (commit/pipeline.rs:354) is turnAt.
\*   nearTo, wokenTo: the orders named as near, and the orders told so.
\*       Turns.near_to and Turns.woken_to (commit/turn_wait.rs:54,
\*       commit/turn_wait.rs:56).
\*   nearMark[i]: order i was told its turn is near. The NEAR flag
\*       (commit/descriptor.rs:365), set by wake_near's mark
\*       (commit/pipeline.rs:806).
\*   owes[a]: thread a's commit is done and it still has to tell. drive's call
\*       of wake_near once publish_upto says Published (commit/pipeline.rs:1067).
\*   Lead: how far past the next order a thread's own must be before it parks
\*       far from its turn. Turns::lead (commit/turn_wait.rs:73): LEAD
\*       (commit/turn_wait.rs:41), and one more for every LEAD_PER_SLEEPER
\*       sleepers.
\*   Patience: a sleeper may give up waiting. PATIENCE for a record
\*       (commit/write_unit.rs:146), TURN_PATIENCE far from the turn
\*       (commit/turn_wait.rs:46).
\*
\*   EncodeStart(a, i), EncodeEnd(a): claim the encoding of a record nobody is
\*       encoding, encode it, set FILLED and wake the parked. RegolithEngine::fill
\*       (commit/write_unit.rs:581), called by drive (commit/pipeline.rs:1043) for
\*       the owner's own record, by the allocations (commit/allocate.rs:91), and
\*       by bring_written for a record a thread needs that nobody is encoding
\*       (commit/write_unit.rs:628); Descriptor::fill and its mark
\*       (commit/descriptor.rs:566).
\*   DirectStart(a): the fast path (WU8). An owner whose record nobody has in
\*       hand finds the unit free with no note on it, encodes its record and goes
\*       to write it by itself; StealStart then counts it in and looks again.
\*       RegolithEngine::write_alone (commit/write_unit.rs:775), called by drive
\*       and drive_nowait before the unit's way (commit/pipeline.rs:1023):
\*       WriteUnit::quiet (commit/write_unit.rs:213), one load; then fill and
\*       copy_record. The unit does not see such writes, so several owners may
\*       take the fast path at once, each writing its own record.
\*   Gather(a): claim the free unit and collect the first run. try_batch
\*       (commit/write_unit.rs:849): try_claim, then the scan from past the
\*       written hint or the published frontier, whichever is further.
\*   RunStart(a): count in, look again, keep what still waits.
\*       log.count_in(run.epoch) (commit/write_unit.rs:914), then the look again,
\*       which is Kept.
\*   WriteLand(a), a run: the run's one write lands. flight.write
\*       (commit/write_unit.rs:935) through InFlight::write (wal_log.rs:650),
\*       LogFile::write_all_vectored_at and its pwritev, or pwrite for a run of
\*       one (../env/std_env.rs:479); then the count out and the quiet marks
\*       (mark_quiet). The run's sleepers are woken only after the let go
\*       (Release, WU7).
\*   WriteLand(a), a copy: copy_record (commit/write_unit.rs:793) through
\*       Log::write_unless (wal_log.rs:391), then Descriptor::mark, which wakes.
\*   LandFail(a): a write fails. The error arms of try_batch and copy_record:
\*       the log is poisoned and the parked are woken.
\*   Release(a): let the unit go and tell. Claim's drop
\*       (commit/write_unit.rs:1027): WriteUnit::release reads the notes in its
\*       swap, then tell_waiters (commit/write_unit.rs:999) wakes every record
\*       still waiting for the unit (WriteUnit::tell, wake_if_waiting
\*       (commit/write_unit.rs:522)); then publish_ready, then the run's own
\*       sleepers. Mutant FirstOnly is the code as it was: the first record only,
\*       the note put back on a free unit.
\*   PubHorizon(a), PubFrontier(a) with PublishesRun(a, p): the holder
\*       publishes what its run made publishable before it wakes the run's owners
\*       (WU7). publish_ready (commit/pipeline.rs:937), called after the let go
\*       (commit/write_unit.rs:981); each order through publishable_slot and
\*       publish_slot (commit/pipeline.rs:914), as every publisher does.
\*   ParkLook(a), ParkSleep(a): find the record in another's hands, then park
\*       on it. bring_written's try_batch returns the claim it saw; park_on
\*       (commit/write_unit.rs:736) then registers on the record
\*       (Descriptor::register (commit/descriptor.rs:658)) and looks again, its
\*       note on the claim it saw part of the look; a refused note means the claim
\*       is gone, and the thread looks afresh. Mutant NoteTrustsALoad is the code
\*       as it was: a note trusted off a plain load.
\*   CopyOwn(a): a commit that waits for a sync finds its own record in
\*       another's hands and copies it instead of sleeping on it. drive's
\*       bring_written with Await::Copy for an Immediate slot
\*       (commit/pipeline.rs:1057), the same path the round's holder takes.
\*   Timeout(a): the park's time runs out. park_timeout in Descriptor::park,
\*       and out_of_patience (commit/write_unit.rs:708) saying yes.
\*   StealStart(a): the copy after the patience. copy_record, through
\*       Log::write_unless: count in, look again (the `written` test), write.
\*   WriteStart(a, i): the round's holder copies. sync_written calls
\*       bring_written with Await::Copy (commit/round.rs:243).
\*   Need(a): the record a thread needs next. drive's bring_written of its own
\*       record (commit/pipeline.rs:1062), then publish_upto's of the next to
\*       publish (commit/pipeline.rs:898).
\*   Usable, RunFrom, Firsts: which records a run takes. The `usable` test of
\*       the scan and Run::take's same log, same epoch, offset that follows on.
\*   Pass, Void, DecideNow, UnitRun with Named(i): a frontier pass names the
\*       orders whose turn is now near. RegolithEngine::pass_decided
\*       (commit/pipeline.rs:786): Ring::pass_decided (commit/ring.rs:246), then
\*       Turns::name (commit/turn_wait.rs:79); the void path and the ordered units
\*       pass through the same function.
\*   RbFinish with Named and its wakes: a rollback's jump names the orders
\*       past it, thaw_decided then Turns::name (commit/ordered.rs:790); an owner
\*       asleep far from its turn is woken when its order is thrown out (FAILED
\*       wakes the slot's sleepers) or lands (Descriptor::land).
\*   TurnPark(a): a blocking thread far from its turn, finding the next order
\*       being decided by another (its DECIDING claim taken), parks on its own
\*       order. help_decide (commit/pipeline.rs:422), then park_far
\*       (commit/pipeline.rs:816): Turns::count_in (commit/turn_wait.rs:111),
\*       Descriptor::park with Turns::near (commit/turn_wait.rs:125) as its look.
\*   TurnWake(a): the sleeper wakes: NEAR set, or TURN_PATIENCE passed; then
\*       Turns::count_out and help_decide looks again.
\*   WakeNear(a): a finished commit tells the named orders. wake_near
\*       (commit/pipeline.rs:798): Turns::claim_told (commit/turn_wait.rs:92),
\*       then mark(NEAR) on each order in the ring.
\*   PubFrontier waking a told owner from turnAsleep: a blocking slot's
\*       landing wakes whoever parked on it. Descriptor::land
\*       (commit/descriptor.rs:483), called by publish_slot and by the rollback.
\*
\*   OneClaim (WU1): try_claim's compare-and-swap; loom
\*       one_thread_at_a_time_holds_the_unit (commit/loom_write_unit.rs).
\*   OneEncoder (D4): Descriptor::fill's claim of FILLING; loom
\*       one_thread_encodes_a_record, its calibration
\*       an_encoding_claimed_with_a_load_and_a_store_writes_one_buffer_twice; RED
\*       EncodeNoClaim.
\*   NoWriteUnderSync (WG4, WU3): the gate's settle (wal_gate.rs:242) against
\*       a write's enter (wal_gate.rs:144) and its look again; loom
\*       a_run_never_lands_under_a_sync.
\*   NoWriteUnderSync on the fast path (WU8): copy_record's look again once
\*       counted in, the same as any copy; RED DirectNoRecheck. Without its guard
\*       the fast path stays safe (row Green_Direct_AnyTime): the guard is about
\*       speed. Loom the_fast_path_racing_a_claimer_writes_only_whole_records and
\*       its calibration.
\*   DurableBeforeVisible through the holder's publishing (WU7): publish_ready
\*       asks publishable_slot like every publisher; RED RunSkipsCheck.
\*   NoLiveThreadBlocked with Patience off (WU4, no lost wake): the mark and
\*       the park's flag share one word; a note lands only on the claim it was
\*       meant for, and the let go wakes every waiting record. Rows
\*       Green_Copies_Woken; RED LostWake (nobody told) and RED NoteTrustsALoad
\*       (three threads). Loom two_writers_behind_a_holder_are_never_left_asleep
\*       on the production note, park, release, tell and wake, with the old note
\*       and the old one-record wake as its calibrations;
\*       a_parked_writer_is_woken_when_its_record_is_written;
\*       a_writer_parked_on_a_record_being_encoded_is_woken_when_it_is_filled.
\*   NoLiveThreadBlocked with Patience off, far from the turn (P6, T1 to T3):
\*       the count against the pass, each behind a handshake; the claim of the
\*       named orders; the landing's wake. Rows Green_Turn, RED TurnNeverTold and
\*       RED LeadZero; loom a_call_far_from_its_turn_is_named_and_told and
\*       a_call_far_from_its_turn_is_woken_when_its_slot_lands
\*       (commit/loom_turn_wait.rs), with their calibrations.
\*   EveryLiveCommitCompletes with a stopped holder (WU5) and with a sleeper
\*       nobody tells: the patience. Lean a_park_is_bounded,
\*       patience_bounds_the_wait, encoding_never_adds and a_turn_park_is_bounded
\*       (Regolith/PipelineHelping.lean).
\*
\* RUST WITH NO STEP OF ITS OWN HERE, and why that loses nothing:
\*   - the spin before a park (commit/write_unit.rs:688) and its budget
\*     SPIN_BUDGET (commit/write_unit.rs:143), past which a waiter parks at
\*     once: a wait that ends by itself changes nothing, and which waiters
\*     spin first changes nothing either;
\*   - the written hint WriteUnit.written (commit/write_unit.rs:165): it only
\*     skips records that are written, which the model's scan passes over too;
\*   - the run's caps MAX_BATCH_RECORDS and MAX_BATCH_BYTES
\*     (commit/write_unit.rs:132): a run that stops early is a run whose next
\*     record is not usable, which the model has;
\*   - the logical ends of a park (FRONTIER_PATIENCE, the ring full): more
\*     ways for Timeout to happen, which may happen at any time anyway;
\*   - the lead growing with the sleepers (LEAD_PER_SLEEPER): a larger lead
\*     only names orders sooner; the rows take Lead as a constant, 1 where a
\*     thread must be told and 0 where parks happen as eagerly as can be and
\*     patience wakes them;
\*   - a turn sleeper woken by its record filled or written, or by its slot
\*     failed: wakes the model leaves out, so it shows that the ones it keeps
\*     are enough;
\*   - parked_at belongs to one call of help_decide, turnAt to the thread: a
\*     thread that leaves help_decide for a unit and comes back may park once
\*     more in the code, which only patience in the model would allow; such a
\*     park is bounded the same way (a_turn_park_is_bounded);
\*   - the round's holder may encode a record nobody is encoding before it
\*     copies it (bring_written with Await::Copy): its copy in the model needs
\*     no fill;
\*   - the line job of a commit_nowait commit (queue_on, tell_line):
\*     commit_nowait is not in this model (see WHAT THE MODEL LEAVES OUT); its
\*     handshake is the loom model
\*     a_queued_commit_is_told_when_the_unit_is_let_go.
\*   - MAX_PARKS (commit/write_unit.rs:156): a woken thread may park again,
\*     and its patience runs from its first wait; the model's Timeout may come
\*     at any moment, which covers every such bound;
\*   - write_alone losing the race for the encoding (Descriptor::fill returns
\*     Ok(false) when another thread claimed FILLING first): copy_record then
\*     encodes the same bytes on its own thread, a copy the model has as
\*     StealStart;
\*   - bring_written's copy for a commit_nowait commit (Await::Leave with an
\*     unfilled record): commit_nowait is not in this model;
\*   - a run's failed write wakes its sleepers after the let go in the code,
\*     at the failure in the model (LandFail): the same threads, awake to find
\*     the log failed;
\*   - WakeNear's claim of the named range and its marks are separate in the
\*     code and one step here: a mark only adds NEAR, so late, repeated or
\*     reordered marks change nothing, and a thread that stops between them
\*     leaves those orders to their patience.
\* MODEL WITH NO RUST: none. Every step and variable above is code.
\*
\* THE MODEL IS IN THREE MODULES, each small enough to read in one go:
\*   CommitPipelineBase.tla   what a configuration chooses, the state, and
\*                            the small questions the steps ask;
\*   CommitPipelineSteps.tla  the steps, one action each;
\*   CommitPipeline.tla       this one: what must be true of them.

\* The state, the helpers and the steps.
EXTENDS CommitPipelineSteps

\* Invariants: what is true in every reachable state.

\* Every variable holds the kind of thing its comment says.
TypeOK ==
  \* The tail is a ticket number or zero.
  /\ tail \in 0..MaxSlots
  \* Cells hold one of the four states.
  /\ cell \in [Slots -> {"free", "claimed", "ready", "void"}]
  \* Owners are threads, or nobody.
  /\ owner \in [Slots -> Actors \cup {0}]
  \* Verdicts are one of the four.
  /\ dec \in [Slots -> {"none", "commit", "abort", "unit"}]
  \* Sequences and epochs are numbers...
  /\ sq \in [Slots -> Nat] /\ ep \in [Slots -> Nat]
  \* ...and so are log generations and memtable numbers.
  /\ logOf \in [Slots -> Nat] /\ memOf \in [Slots -> Nat]
  \* The three flags are yes or no.
  /\ ap \in [Slots -> BOOLEAN] /\ wr \in [Slots -> BOOLEAN] /\ failed \in [Slots -> BOOLEAN]
  \* The frontiers lie within the tickets handed out, published behind decided.
  /\ dto \in 0..tail /\ pub \in 0..dto /\ tipIdx \in 0..tail
  \* The switches are yes or no.
  /\ frozen \in BOOLEAN /\ poisoned \in BOOLEAN
  \* The round's holders are threads, and so is a claimant.
  /\ round \subseteq Actors /\ claimant \in Actors \cup {0}
  \* A sync's promise is a sequence, or zero for none.
  /\ target \in [Actors -> Nat]
  \* Every thread is at a known point.
  /\ pc \in [Actors -> {"idle", "open", "begun", "claimed", "ready", "done", "failed", "aborted"}]
  \* The stopped are threads.
  /\ stopped \subseteq Actors
  \* The writes in flight are sets of orders: one for a copy, several for
  \* a run.
  /\ fly \in [Actors -> SUBSET Slots]
  \* A record is filled or not.
  /\ filled \in [Slots -> BOOLEAN]
  \* The write unit is a thread's, or nobody's.
  /\ writer \in Actors \cup {0}
  \* A gathered run is a set of orders.
  /\ gathered \in [Actors -> SUBSET Slots]
  \* The parked are threads...
  /\ asleep \subseteq Actors
  \* ...each on a record, or 0.
  /\ sleepOn \in [Actors -> 0..MaxSlots]
  \* A thread out of patience names the record it will write, or 0.
  /\ late \in [Actors -> 0..MaxSlots]
  \* The request to be woken at the next claim is yes or no.
  /\ wanted \in BOOLEAN
  \* An encoding claim is yes or no for each record...
  /\ filling \in [Slots -> BOOLEAN]
  \* ...and a thread has one record in hand, or none.
  /\ encoding \in [Actors -> 0..MaxSlots]
  \* A run's end is an order, or 0.
  /\ runEnd \in [Actors -> 0..MaxSlots]
  \* The threads parked far from their turn are threads...
  /\ turnAsleep \subseteq Actors
  \* ...each remembering where the frontier stood, or 0.
  /\ turnAt \in [Actors -> 0..(MaxSlots + 1)]
  \* The orders named and told are ticket numbers, or 0, or up to Lead past
  \* the last ticket.
  /\ nearTo \in 0..(MaxSlots + Lead) /\ wokenTo \in 0..(MaxSlots + Lead)
  \* Each order is marked near or not...
  /\ nearMark \in [Slots -> BOOLEAN]
  \* ...and each thread owes a telling or not.
  /\ owes \in [Actors -> BOOLEAN]
  \* The claims are counted, each thread saw one of them or none...
  /\ claimNo \in Nat /\ saw \in [Actors -> Nat]
  \* ...the noted ones are claim numbers...
  /\ \A n \in noted : n \in Nat
  \* ...and a thread is about to park on one record, or none.
  /\ looking \in [Actors -> 0..MaxSlots]
  \* A cell is free exactly when its ticket has not been handed out.
  /\ \A i \in Slots : (i <= tail) <=> cell[i] # "free"

\* R1, one fate per ticket: an owner who believes its order is in the ring
\* is right. Fails under VoidNotCas.
OwnerSeesItsFate ==
  \* Every thread that takes its order to be in the ring finds it there.
  \A a \in Actors : pc[a] \in {"ready", "done", "failed"} => cell[mine[a]] = "ready"

\* One thread at a time on the log's device step.
OneRound == Cardinality(round) <= 1

\* Orders are settled in ticket order: everything up to the frontier has a
\* verdict or was thrown away, and nothing beyond the next order has one.
DecidedIsPrefix ==
  \* Up to the frontier, settled.
  /\ \A i \in 1..dto : cell[i] = "void" \/ dec[i] # "none"
  \* Beyond the next order, untouched.
  /\ \A i \in Slots : i > dto + 1 => dec[i] = "none"
  \* The tip is never more than one order ahead of the frontier.
  /\ tipIdx <= dto + 1

\* No hole: the sequences held by the orders the tip covers are exactly
\* 1, 2, ..., up to the last one handed out, and only an order that landed
\* holds one. An abort, a thrown-away ticket and a thrown-out order hold
\* none. Fails under AbortHole, SettleAnyEpoch and NoFreeze.
NoHole ==
  \* The sequences that count are exactly 1..tipNext.
  /\ {sq[i] : i \in {j \in 1..tipIdx : Takes(j)}} = 1..tipNext
  \* Only a commit or a unit holds a sequence that counts.
  /\ \A i \in 1..tipIdx : Takes(i) => dec[i] \in {"commit", "unit"}

\* Sequence order is ticket order.
SeqInRingOrder ==
  \* Of two orders that count, the earlier ticket holds the smaller sequence.
  \A i, j \in 1..tipIdx : i < j /\ Takes(i) /\ Takes(j) => sq[i] < sq[j]

\* P2: an order the line has moved past that landed is in the memtable, so
\* the order after it was checked against it. Fails under PassBeforeApply.
AppliedBeforePassed ==
  \* Every order behind the frontier that landed and counts is in the memtable.
  \A i \in 1..dto : (Brings(i) \/ dec[i] = "unit") /\ ~failed[i] => InMem(i)

\* Outside a rollback, the memtable readers are shown holds no order that
\* was thrown out. Fails under KeepMemtable.
MemtableOnlyLive == rbLast = 0 => \A i \in Slots : InMem(i) => ~failed[i]

\* The same for the log: no record of a thrown-out order. Fails under
\* NoEpochGate.
RecordsAreLive == rbLast = 0 => \A i \in Slots : wr[i] => ~failed[i]

\* Everything behind the published frontier is settled and was allowed to
\* be finished. Fails under RollbackForgets.
PublishedPrefix == \A i \in 1..pub : Publishable(i)

\* P3: an order behind the published frontier is visible. Its owner, told
\* it is finished, reads its own write. Fails under FrontierFirst.
PublishedIsVisible == \A i \in 1..pub : Takes(i) => sq[i] <= visible

\* No reader sees a later order without every earlier one: whatever the
\* horizon covers is in the memtable, is the next order to finish at most,
\* and was allowed to be finished. Fails under OutOfOrder.
ReadersSeePrefix ==
  \* For every order that counts and that the horizon covers:
  \A i \in 1..tipIdx : Takes(i) /\ sq[i] <= visible =>
    \* Its writes are there to be read.
    /\ InMem(i)
    \* It is finished, or the very next to be.
    /\ i <= pub + 1
    \* And it met the conditions for finishing.
    /\ Publishable(i)

\* What the durable mark covers is written. Fails under SyncPastGap,
\* TwoSyncs and UnitNoDrain.
DurableImpliesWritten ==
  \* Every commit the durable mark covers has its record written.
  \A i \in 1..tipIdx : Takes(i) /\ dec[i] = "commit" /\ sq[i] <= durable => wr[i]

\* The durable mark never passes the last sequence handed out: a rollback
\* hands back no sequence a sync vouched for. Fails under TwoSyncs.
DurableWithinDecided == durable <= tipNext

\* At Immediate, visible only once durable.
DurableBeforeVisible ==
  \* For every order the tip covers:
  \A i \in 1..tipIdx :
    \* a commit that asked to wait for the disk and is visible is durable.
    Takes(i) /\ dec[i] = "commit" /\ owner[i] \in Immediate /\ sq[i] <= visible => sq[i] <= durable

\* No lost update: a transaction that commits read no key an earlier
\* landed order wrote after it began reading. Of two transactions that
\* read and write one key from the same snapshot, at most one commits.
\* Fails under ValidatePublished, PassBeforeApply and TrustEarly.
NoLostUpdate ==
  \* For every two orders the tip covers:
  \A i, j \in 1..tipIdx :
    \* Two orders in ticket order...
    /\ i < j
    \* ...the earlier one landed and counts...
    /\ Takes(i)
    \* ...the later one a transaction that committed and counts...
    /\ dec[j] = "commit" /\ ~failed[j] /\ owner[j] \in Committers \ Plain
    \* ...and read a key the earlier one wrote.
    /\ Writes[owner[i]] \cap Reads[owner[j]] # {}
    \* Then the later one began reading after the earlier one landed.
    => sq[i] <= snap[owner[j]]

\* No transaction is turned away for nothing: one that aborted, in its
\* slot or at its early check, lost to an order that landed after it began
\* reading and wrote a key it read. With NoLostUpdate this says the
\* pipeline decides each transaction as committing them one at a time, in
\* sequence order, would.
AbortsHaveCause ==
  \* An abort decided in a slot that still counts...
  /\ \A i \in 1..tipIdx : dec[i] = "abort" /\ ~failed[i] =>
       \* ...has an earlier order that landed...
       \E j \in 1..(i - 1) : /\ Takes(j)
                              \* ...after its transaction began reading...
                              /\ sq[j] > snap[owner[i]]
                              \* ...on a key it read.
                              /\ Writes[owner[j]] \cap Reads[owner[i]] # {}
  \* An abort at the early check...
  /\ \A a \in Actors : pc[a] = "aborted" =>
       \* ...has an order that landed...
       \E j \in 1..tipIdx : /\ Takes(j)
                             \* ...after the transaction began reading...
                             /\ sq[j] > snap[a]
                             \* ...on a key it read.
                             /\ Writes[owner[j]] \cap Reads[a] # {}

\* A rotation runs alone: a commit still in flight belongs to the log that
\* is current, so a rotation never leaves a record to be written in a log
\* it sealed. Fails under UnitNoDrain.
InFlightInCurrentLog ==
  \* Every unfinished commit that counts belongs to the log that is current.
  \A i \in 1..tipIdx : i > pub /\ Brings(i) /\ ~failed[i] => logOf[i] = gen

\* G2: nobody is told "done" for an order that was thrown out, nobody is
\* told "failed" for one that landed, and a commit told "done" has its
\* record in the log.
NoFalseAck ==
  \* For every thread, what it was told is true:
  \A a \in Actors :
    \* "done" means the order counts...
    /\ (pc[a] = "done" => ~failed[mine[a]])
    \* ...and, if it committed with writes, that its record is written.
    /\ (pc[a] = "done" /\ Brings(mine[a]) => wr[mine[a]])
    \* "failed" means it was thrown out.
    /\ (pc[a] = "failed" => failed[mine[a]])

\* E21, bounded work: a thread works out a verdict only for orders up to
\* its own, so no caller does the work of an unbounded line of others. Rules
\* out: thread 1's order is decided and waits for its sync; meanwhile thread
\* 1 goes on deciding the orders that keep arriving behind it, and its call
\* returns only when nobody commits any more. Fails under WorkBeyond. Lean:
\* helping_is_bounded (Regolith/PipelineHelping.lean).
WorksUpToItsOwn ==
  \* A thread's note names a ticket at or before its own.
  \A a \in Actors : seen[a] # NoNote => seen[a][1] <= mine[a]

\* A note is only ever for an order with no verdict: the others were used
\* up when the verdict was written.
NotesAreForUndecided ==
  \* The ticket a note names has no verdict.
  \A a \in Actors : seen[a] # NoNote => dec[seen[a][1]] = "none"

\* No live thread is ever stuck: when no live thread can take a step,
\* every live thread's call has returned. Fails under NoHelping and
\* StopHoldsClaim.
NoLiveThreadBlocked ==
  \* With no step left for any live thread, each of them has its answer.
  (~ENABLED LiveNext) => \A a \in Actors \ stopped : pc[a] \in {"done", "failed", "aborted"}
                                                    \* ...and is awake to take it home.
                                                    /\ a \notin asleep /\ a \notin turnAsleep
                                                    \* (...and not about to park.)
                                                    /\ looking[a] = 0

\* WG4: no write lands on a range of the log after a sync that covers it has
\* begun. A sector in flight when the power goes may come back garbled
\* whatever bytes were being written to it, so a late copy of a record, the
\* very bytes already there, must not be in flight over what a sync made
\* durable or is making durable. Tiny example: the owner of order 1 is slow
\* inside its write; order 2's thread writes order 1's record for it, then
\* its own, claims the round and syncs both; the owner's copy lands after
\* the sync began, over bytes it vouched for. Fails under SyncNoWait.
\* Checked by TLC on the rows that set Copies, and on the code by the loom
\* models in src/engine/loom_model/park.rs and
\* src/engine/commit/loom_write_unit.rs (a run, which looks again once
\* counted in) and the tests under the fault shim in
\* src/engine/commit/late_copy_tests.rs; it has no Lean statement.
NoWriteUnderSync ==
  \* Every record of every write in flight, a copy or a run...
  \A b \in Actors : \A j \in fly[b] :
     \* ...is one no completed sync made durable...
     /\ sq[j] > durable
     \* ...and no sync under way vouches for.
     /\ \A a \in round : target[a] = 0 \/ sq[j] > target[a]

\* WU1: one holder of the write unit. Only the holder has a run gathered,
\* and only it has more than one record in flight; every other thread
\* writes one record at a time, as a copy. Rules out: two threads each
\* gathered a run and both write it, which is the contention on the file's
\* lock the unit exists to end, and with a record in both runs, two copies
\* of it in flight that nobody counted on.
OneClaim ==
  \* A gathered run is the holder's.
  /\ \A a \in Actors : gathered[a] # {} => writer = a
  \* So is a write of more than one record.
  /\ \A a \in Actors : Cardinality(fly[a]) > 1 => writer = a

\* The run's ticket bound never stops a live thread, so nothing above is an
\* artefact of the bound.
BoundNeverBinds ==
  \* A live thread that still has to pull a ticket finds one left.
  \A a \in Actors \ stopped : pc[a] \in {"idle", "open", "begun"} => tail < MaxSlots

----------------------------------------------------------------------------
\* Witnesses. Each of these is FALSE somewhere in the design: a run that
\* checks one as an invariant must break it. That shows the GREEN runs
\* really reach the case, and are not green because nothing happened.

\* "No order is ever thrown out." Broken once a rollback discards one.
NothingThrownOut == \A i \in Slots : ~failed[i]

\* "No verdict is ever found stale." Broken once an order is failed at its
\* settling, ahead of the tip, because its verdict came from a state a
\* rollback had discarded.
NoStaleVerdict == \A i \in Slots : failed[i] => i <= tipIdx

\* "No ticket is ever thrown away." Broken once a helper voids one.
NothingVoided == \A i \in Slots : cell[i] # "void"

\* "A stopped thread's order is never finished." Broken once the others
\* have carried it through.
StoppedStaysUnfinished == \A a \in stopped : pc[a] = "ready" => mine[a] > pub

\* "The log is never rotated." Broken once a rotation check seals it.
NeverRotated == gen = 0

\* "No transaction is ever turned away by its own early check." Broken
\* once one is.
NoEarlyAbort == \A a \in Actors : pc[a] # "aborted"

\* "No two threads ever have one record's write in flight." Broken once a
\* thread out of patience has its copy on its way beside the holder's run.
NoTwoCopies == \A a, b \in Actors : a # b => fly[a] \cap fly[b] = {}

\* "No run ever holds two records." Broken once the write unit's holder
\* writes two records with one write.
NoRunOfTwo == \A a \in Actors : Cardinality(fly[a]) <= 1

\* "No thread ever parks on a record." Broken once a writer sleeps until
\* the unit's holder writes its record or lets the unit go.
NeverAsleep == asleep = {}

\* D4: one encoder a record. The FILLING claim lets one thread encode a
\* record into its slot's buffer; two at once would write one buffer
\* together, and the run's holder could send the mixture to the log.
\* Rules out: the owner and a helper both encoding record 1 at once. Fails
\* under EncodeNoClaim.
OneEncoder == \A a, b \in Actors : a # b /\ encoding[a] # 0 => encoding[a] # encoding[b]

\* "Only an owner ever encodes its record." Broken once a thread that needs
\* a record nobody is encoding encodes it itself.
OwnersEncodeOwn == \A a \in Actors : encoding[a] # 0 => encoding[a] = mine[a]

\* A state constraint for the rows that check the write unit's waits with
\* three threads: runs in which a ticket is thrown away are not followed,
\* so nobody needs a second ticket and three tickets are enough. Voided
\* tickets have rows of their own (Green_Live_Voids and the Red rows on
\* voiding); a row that uses this says so.
NoVoidedTicket == \A i \in Slots : cell[i] # "void"

\* "No owner is ever about to write its own record by itself." In a run
\* with no time outs only the fast path does that (or a commit that waits
\* for a sync, copying its record), so this breaks once an owner takes it.
NeverDirect == \A a \in Actors : late[a] = 0 \/ late[a] # mine[a]

\* "No thread ever parks far from its turn." Broken once one does (step 2).
NeverTurnParked == turnAsleep = {}

\* "No run ever ends past its holder's own order." Broken once a holder
\* writes a run that reaches past its own order, which its publishing of
\* the run (WU7) then publishes past its own order too.
NoRunPastOwn == \A a \in Actors : runEnd[a] <= mine[a]

\* "No thread ever runs out of patience." Broken once a parked writer
\* gives up on the holder and writes its record itself.
NeverStolen == \A a \in Actors : late[a] = 0

\* "The write unit is never held by a thread that walked away." Broken once
\* the holder stops for good with the unit claimed, the case the patience
\* exists for.
HolderNeverStops == writer \notin stopped

----------------------------------------------------------------------------
\* Liveness: what eventually happens.

\* Some live thread has a commit in flight.
LivePending == \E a \in Actors \ stopped : pc[a] \in {"begun", "claimed", "ready"}

\* A step that finishes an order: the published frontier moves.
FrontierMoves == pub' > pub

\* Lock-freedom: either orders keep being finished forever, or there comes
\* a time after which no live thread has work left. A run in which a live
\* thread has work and the frontier stops for good is neither.
LockFree == []<><<FrontierMoves>>_vars \/ <>[](~LivePending)

\* A stopped thread blocks nobody: eventually every thread's call has
\* returned, except the threads that walked away.
EveryLiveCommitCompletes ==
  \* At some point every thread has its answer, or walked away.
  <>(\A a \in Actors : pc[a] \in {"done", "failed", "aborted"} \/ a \in stopped)

----------------------------------------------------------------------------
\* Workloads, chosen by the configurations.

\* Thread 1 writes key 1; thread 2 writes keys 1 and 2; any other thread
\* writes key 2. So threads 1 and 2 conflict on key 1, and thread 2
\* conflicts on key 2 with whoever else is there.
Overlapping == [a \in Actors |-> CASE a = 1 -> {1} [] a = 2 -> {1, 2} [] OTHER -> {2}]

\* Nobody shares a key: every commit lands.
Apart == [a \in Actors |-> {a}]

\* What thread 2 writes when it is a transaction that only reads: nothing.
\* Everybody else writes key 1.
OnlyOneWrites == [a \in Actors |-> IF a = 2 THEN {} ELSE {1}]

\* Everybody read key 1.
AllReadOne == [a \in Actors |-> {1}]

====
