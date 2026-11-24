---- MODULE CommitPipelineSteps ----
\* The lock-free commit pipeline: its steps, one action each, in the order
\* a commit meets them. The state they act on is in
\* CommitPipelineBase.tla, and what must be true of them, with the whole
\* story of the model, in CommitPipeline.tla.

\* The state and the helpers.
EXTENDS CommitPipelineBase

\* Actions. Each is one small step of one thread: an add, a
\* compare-and-swap, a write that is safe to repeat, or a store.

\* The start: an empty ring and an empty database.
Init ==
  \* No ticket has been handed out.
  /\ tail     = 0
  \* Every cell is free.
  /\ cell     = [i \in Slots |-> "free"]
  \* Nobody owns a ticket.
  /\ owner    = [i \in Slots |-> 0]
  \* No order has a verdict.
  /\ dec      = [i \in Slots |-> "none"]
  \* No order has a sequence.
  /\ sq       = [i \in Slots |-> 0]
  \* No verdict has an epoch.
  /\ ep       = [i \in Slots |-> 0]
  \* Nothing is in the memtable.
  /\ ap       = [i \in Slots |-> FALSE]
  \* Nothing is in the log.
  /\ wr       = [i \in Slots |-> FALSE]
  \* No write is in flight.
  /\ fly      = [a \in Actors |-> {}]
  \* No record is filled yet.
  /\ filled   = [i \in Slots |-> FALSE]
  \* Nobody holds the write unit.
  /\ writer   = 0
  \* Nobody gathered a run.
  /\ gathered = [a \in Actors |-> {}]
  \* Nobody is parked on a record...
  /\ asleep   = {}
  \* ...so nobody waits for one.
  /\ sleepOn  = [a \in Actors |-> 0]
  \* Nobody ran out of patience.
  /\ late     = [a \in Actors |-> 0]
  \* Nobody asked to be woken when the unit is let go.
  /\ wanted   = FALSE
  \* Nobody is encoding a record...
  /\ filling  = [i \in Slots |-> FALSE]
  \* ...so no thread has one in hand.
  /\ encoding = [a \in Actors |-> 0]
  \* Nobody wrote a run yet.
  /\ runEnd   = [a \in Actors |-> 0]
  \* Nobody is parked far from its turn...
  /\ turnAsleep = {}
  \* ...or ever was.
  /\ turnAt   = [a \in Actors |-> 0]
  \* No order was named...
  /\ nearTo   = 0
  \* ...or told...
  /\ wokenTo  = 0
  \* ...so none is marked near.
  /\ nearMark = [i \in Slots |-> FALSE]
  \* No thread has a commit done yet.
  /\ owes     = [a \in Actors |-> FALSE]
  \* The unit was never claimed...
  /\ claimNo  = 0
  \* ...so no thread saw a claim...
  /\ saw      = [a \in Actors |-> 0]
  \* ...or noted one...
  /\ noted    = {}
  \* ...and none is about to park.
  /\ looking  = [a \in Actors |-> 0]
  \* Nothing was thrown out.
  /\ failed   = [i \in Slots |-> FALSE]
  \* No record belongs to a log yet.
  /\ logOf    = [i \in Slots |-> 0]
  \* No verdict names a memtable yet.
  /\ memOf    = [i \in Slots |-> 0]
  \* Nothing is decided.
  /\ dto      = 0
  \* No rollback is running.
  /\ frozen   = FALSE
  \* The tip is "before the first ticket"...
  /\ tipIdx   = 0
  \* ...with no sequence handed out...
  /\ tipNext  = 0
  \* ...and an empty memtable.
  /\ tipUsed  = 0
  \* No rollback has happened: the pipeline is of epoch zero...
  /\ epoch    = 0
  \* ...the log takes records of epoch zero...
  /\ logEp    = 0
  \* ...and the memtable is the first one.
  /\ memGen   = 0
  \* The first log.
  /\ gen      = 0
  \* Nothing is safe on disk.
  /\ durable  = 0
  \* Nothing is finished.
  /\ pub      = 0
  \* A reader sees nothing.
  /\ visible  = 0
  \* Nobody holds the round.
  /\ round    = {}
  \* No sync is under way.
  /\ target   = [a \in Actors |-> 0]
  \* Nobody claimed a unit.
  /\ claimant = 0
  \* No rollback is running.
  /\ rbLast   = 0
  \* The log is healthy.
  /\ poisoned = FALSE
  \* Nothing has failed.
  /\ fails    = 0
  \* Nobody has started.
  /\ pc       = [a \in Actors |-> "idle"]
  \* No transaction has a snapshot.
  /\ snap     = [a \in Actors |-> 0]
  \* No early check has run.
  /\ hor      = [a \in Actors |-> 0]
  \* Nobody holds a ticket.
  /\ mine     = [a \in Actors |-> 0]
  \* Nobody has read the tip.
  /\ seen     = [a \in Actors |-> NoNote]
  \* Nobody has walked away.
  /\ stopped  = {}

\* Transaction a begins, as a step of its own: it reads at what is visible
\* now, and will ask to commit later.
Begin(a) ==
  \* It is one of the transactions that begin early, and has not begun.
  /\ a \in EarlyBegin /\ pc[a] = "idle"
  \* Its snapshot is the newest sequence a reader may see (a transaction
  \* that shares no key with anybody is never checked, so none is kept).
  /\ snap' = [snap EXCEPT ![a] = IF Contends(a) THEN visible ELSE snap[a]]
  \* It is open.
  /\ pc'   = [pc EXCEPT ![a] = "open"]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, hor, mine, seen, stopped>>

\* Transaction a asks to commit, and its own early check finds that
\* something visible changed one of its keys since it began: it aborts at
\* once, on its own thread, and takes no ticket.
EarlyAbort(a) ==
  \* It is open: it began and has not committed.
  /\ pc[a] = "open"
  \* The early check finds a conflict.
  /\ EarlyConflict(a)
  \* Its call returns with the conflict.
  /\ pc' = [pc EXCEPT ![a] = "aborted"]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, snap, hor, mine, seen, stopped>>

\* Thread a pulls the next ticket: one add on the tail. A transaction
\* that has not begun yet begins here, reading at what is visible now; one
\* that began early has just passed its early check.
Reserve(a) ==
  \* The ticket it is about to get.
  LET i == tail + 1
  \* It has not started, or its last ticket was thrown away, or it is open
  \* and its early check found nothing.
  IN /\ \/ pc[a] = "begun"
        \* (A transaction that begins early does not start here.)
        \/ pc[a] = "idle" /\ a \notin EarlyBegin
        \* (The early check is the step before this one.)
        \/ pc[a] = "open" /\ ~EarlyConflict(a)
     \* The machine still has tickets in this run.
     /\ tail < MaxSlots
     \* A run that names who is first in line gives that thread ticket 1.
     /\ (tail = 0 /\ Head # 0 => a = Head)
     \* The machine moves on by one.
     /\ tail'  = i
     \* The ticket is this thread's.
     /\ owner' = [owner EXCEPT ![i] = a]
     \* The cell is claimed, with no order in it yet.
     /\ cell'  = [cell EXCEPT ![i] = "claimed"]
     \* The thread remembers its ticket.
     /\ mine'  = [mine EXCEPT ![a] = i]
     \* A transaction that starts here takes its snapshot; any other keeps
     \* the one it has. (One that shares no key with anybody is never
     \* checked against anything, so the model keeps no snapshot for it.)
     /\ snap'  = [snap EXCEPT ![a] = IF pc[a] = "idle" /\ Contends(a) THEN visible ELSE snap[a]]
     \* Its early check has covered everything visible now; a thread that
     \* pulls again keeps the horizon it had.
     /\ hor'   = [hor EXCEPT ![a] = IF pc[a] # "begun" /\ Contends(a) THEN visible ELSE hor[a]]
     \* It now has to write its order down.
     /\ pc'    = [pc EXCEPT ![a] = "claimed"]
     \* Nothing else changes.
     /\ UNCHANGED <<fly, writeVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                    claimVars, failVars, seen, stopped>>

\* Thread a writes its order into its cell: the compare-and-swap wins.
Fill(a) ==
  \* It holds a ticket with no order in it.
  /\ pc[a] = "claimed"
  \* Nobody threw the ticket away in the meantime.
  /\ cell[mine[a]] = "claimed"
  \* The order is in the cell.
  /\ cell' = [cell EXCEPT ![mine[a]] = "ready"]
  \* From now on the thread helps.
  /\ pc'   = [pc EXCEPT ![a] = "ready"]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, tail, owner, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub,
                 visible, claimVars, failVars, snap, hor, mine, seen, stopped>>

\* Thread a finds its ticket was thrown away: it must pull another one.
Retry(a) ==
  \* It was about to write its order down...
  /\ pc[a] = "claimed"
  \* ...but a helper voided the cell first.
  /\ cell[mine[a]] = "void"
  \* Back to the ticket machine, with the same snapshot.
  /\ pc' = [pc EXCEPT ![a] = "begun"]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, snap, hor, mine, seen, stopped>>

\* Helper a reaches the next ticket in line and finds no order on it: its
\* owner pulled it and went no further. The helper throws it away, which
\* also settles it. Mutant NoHelping never does this; mutant VoidNotCas
\* throws away a ticket whose order is already written.
Void(a) ==
  \* The next ticket in line.
  LET i == dto + 1
  \* The broken "nobody helps" version has no such step.
  IN /\ Mutant # "NoHelping"
     \* The thread is helping, and not parked behind a claimed unit...
     /\ Helping(a) /\ ~Parked(a)
     \* ...and the ticket is ahead of its own.
     /\ i < mine[a]
     \* No rollback holds the frontier.
     /\ ~frozen
     \* The cell has a ticket and no order...
     /\ \/ cell[i] = "claimed"
        \* ...(the broken version does not look, and also takes one whose
        \* order is in but not yet decided).
        \/ Mutant = "VoidNotCas" /\ cell[i] = "ready" /\ dec[i] = "none"
     \* The ticket is thrown away.
     /\ cell' = [cell EXCEPT ![i] = "void"]
     \* The line moves past it.
     /\ dto'  = i
     \* The pass names the order whose turn is now near (step 2).
     /\ nearTo' = Named(i)
     \* Nothing else changes.
     /\ UNCHANGED <<fly, unitVars, wokenTo, nearMark, owes, tail, owner, decVars, ap, wr, failed, tipVars, epochVars, durable, pub, visible,
                    claimVars, failVars, threadVars>>

\* Thread a looks at the next order in line and notes down the tip: the
\* state it will work the verdict out from. (A thread that dawdles between
\* this and writing the verdict matters only if a rollback can fall in
\* between. A configuration that sets Coarse takes the two as one step,
\* DecideNow, once no rollback can.)
ReadTip(a) ==
  \* The next order can be decided by this thread.
  /\ CanDecide(a)
  \* The steps are taken one by one.
  /\ SmallSteps
  \* The thread is not in the middle of another verdict.
  /\ seen[a] = NoNote
  \* It notes the ticket, the last sequence, the epoch and the memtable.
  /\ seen' = [seen EXCEPT ![a] = <<DecideSlot(a), tipNext, epoch, memGen>>]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub,
                 visible, claimVars, failVars, pc, snap, hor, mine, stopped>>

\* Thread a writes the verdict it worked out from its note, with one
\* compare-and-swap: if another thread's verdict is already there, that one
\* stands and this one is dropped. A commit takes the next sequence; an
\* abort takes nothing (mutant AbortHole: it takes one too).
DecideCas(a) ==
  \* What the thread's note says. The ticket it is for...
  LET i  == seen[a][1]
      \* ...the last sequence handed out when it was made...
      nx == seen[a][2]
      \* ...the pipeline's epoch then...
      e  == seen[a][3]
      \* ...and the memtable in use then.
      m  == seen[a][4]
  \* The thread has a note.
  IN /\ seen[a] # NoNote
     \* It is still here.
     /\ Live(a)
     \* If there is no verdict yet, this one is written...
     /\ IF dec[i] = "none"
          \* ...commit or abort...
          THEN /\ dec'   = [dec EXCEPT ![i] = VerdictOf(i, nx)]
               \* ...with the next sequence if it takes one...
               /\ sq'    = [sq EXCEPT ![i] = SeqOf(i, nx)]
               \* ...stamped with the epoch of the state it came from...
               /\ ep'    = [ep EXCEPT ![i] = e]
               \* ...its record belongs to the current log...
               /\ logOf' = [logOf EXCEPT ![i] = gen]
               \* ...and its writes go to the memtable the note names.
               /\ memOf' = [memOf EXCEPT ![i] = m]
          \* Otherwise the other thread's verdict stands.
          ELSE UNCHANGED decVars
     \* Every note for this ticket is used up: the others would lose the
     \* compare-and-swap, which changes nothing.
     /\ seen' = [b \in Actors |-> IF seen[b][1] = i THEN NoNote ELSE seen[b]]
     \* Nothing else changes.
     /\ UNCHANGED <<fly, writeVars, ringVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                    claimVars, failVars, pc, snap, hor, mine, stopped>>

\* Thread a decides the next order in one go, when no rollback can fall
\* between the steps: the verdict, its writes into the memtable, the tip
\* and the frontier.
DecideNow(a) ==
  \* The ticket to decide: the next in line.
  LET i == DecideSlot(a)
  \* The next order can be decided by this thread.
  IN /\ CanDecide(a)
     \* The steps are taken in one go.
     /\ ~SmallSteps
     \* The verdict is written: commit or abort...
     /\ dec'     = [dec EXCEPT ![i] = VerdictOf(i, tipNext)]
     \* ...with the next sequence if it takes one...
     /\ sq'      = [sq EXCEPT ![i] = SeqOf(i, tipNext)]
     \* ...of the current state...
     /\ ep'      = [ep EXCEPT ![i] = epoch]
     \* ...of the current log...
     /\ logOf'   = [logOf EXCEPT ![i] = gen]
     \* ...and of the current memtable.
     /\ memOf'   = [memOf EXCEPT ![i] = memGen]
     \* A commit's writes, if it brought any, go into the memtable.
     /\ ap'      = [ap EXCEPT ![i] = ~Loses(i, tipNext) /\ Writes[owner[i]] # {}]
     \* The tip is this order...
     /\ tipIdx'  = i
     \* ...with the sequence it took, if any, the last handed out...
     /\ tipNext' = IF SeqOf(i, tipNext) # 0 THEN SeqOf(i, tipNext) ELSE tipNext
     \* ...and a commit that brought writes charged to the memtable.
     /\ tipUsed' = IF ~Loses(i, tipNext) /\ Writes[owner[i]] # {} THEN tipUsed + 1 ELSE tipUsed
     \* The line moves past it.
     /\ dto'     = i
     \* A note another thread made for this ticket before the last rollback
     \* is used up: its compare-and-swap would lose.
     /\ seen'    = [b \in Actors |-> IF seen[b][1] = i THEN NoNote ELSE seen[b]]
     \* The pass names the order whose turn is now near (step 2).
     /\ nearTo'  = Named(i)
     \* Nothing else changes.
     /\ UNCHANGED <<fly, unitVars, wokenTo, nearMark, owes, ringVars, wr, failed, epochVars, durable, pub, visible, claimVars, failVars,
                    pc, snap, hor, mine, stopped>>

\* Thread a puts order i's writes into the memtable its verdict names.
\* That is the memtable readers are shown unless a rollback replaced it
\* since (the thread does not look: it may have read the tip long ago).
\* This comes before the line moves past the order, so the order after it
\* is checked against these writes.
Apply(a, i) ==
  \* It is a commit that brought writes...
  /\ Brings(i)
  \* ...and they are not in yet.
  /\ ~ap[i]
  \* It is the order being settled, the next in line, and the tip has not
  \* moved to it yet...
  /\ \/ i = dto + 1 /\ tipIdx < i
     \* ...(mutant PassBeforeApply: any order the line has passed too,
     \* however late).
     \/ Mutant = "PassBeforeApply" /\ i <= dto
  \* The thread may work on it.
  /\ MayWork(a, i)
  \* The writes are in the memtable the verdict names.
  /\ ap' = [ap EXCEPT ![i] = TRUE]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, threadVars>>

\* Thread a moves the tip to the next order's verdict: from now on the
\* pipeline's state is the state after that order. Only a verdict of the
\* current epoch may become the tip (mutant SettleAnyEpoch: any), and a
\* commit only once applied (mutant PassBeforeApply: at once).
TipMove(a) ==
  \* The order being settled.
  LET i == dto + 1
  \* It exists.
  IN /\ i <= tail
     \* It has a verdict.
     /\ dec[i] # "none"
     \* The tip is still before it.
     /\ tipIdx < i
     \* The verdict is of the current state.
     /\ (ep[i] = epoch \/ Mutant = "SettleAnyEpoch")
     \* A commit's writes are in the memtable first.
     /\ (Brings(i) => ap[i] \/ Mutant = "PassBeforeApply")
     \* The thread may work on it.
     /\ MayWork(a, i)
     \* The tip is this order now...
     /\ tipIdx'  = i
     \* ...the last sequence is the one it took, if it took one...
     /\ tipNext' = IF sq[i] # 0 THEN sq[i] ELSE tipNext
     \* ...and a commit that brought writes is charged to the memtable.
     /\ tipUsed' = IF Brings(i) THEN tipUsed + 1 ELSE tipUsed
     \* Nothing else changes.
     /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, epochVars, durable, pub, visible,
                    claimVars, failVars, threadVars>>

\* Thread a finds the next order's verdict came from a state a rollback
\* has since thrown away: nothing of it may be used, so the order is
\* failed. (Mutant SettleAnyEpoch never notices.)
MarkStale(a) ==
  \* The order being settled.
  LET i == dto + 1
  \* It exists.
  IN /\ i <= tail
     \* It has a verdict...
     /\ dec[i] # "none"
     \* ...the tip is still before it...
     /\ tipIdx < i
     \* ...and the verdict's epoch is not the current one.
     /\ ep[i] # epoch
     \* The broken version skips this check.
     /\ Mutant # "SettleAnyEpoch"
     \* It is not failed already.
     /\ ~failed[i]
     \* The thread may work on it.
     /\ MayWork(a, i)
     \* The order is thrown out.
     /\ failed' = [failed EXCEPT ![i] = TRUE]
     \* Nothing else changes.
     /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, dto, tipVars, epochVars, durable, pub, visible,
                    claimVars, failVars, threadVars>>

\* Thread a moves the line past the next order: it is settled, either as
\* the tip or as a failed order.
Pass(a) ==
  \* The order being settled.
  LET i == dto + 1
  \* It exists.
  IN /\ i <= tail
     \* No rollback holds the frontier.
     /\ ~frozen
     \* It has a verdict.
     /\ dec[i] # "none"
     \* The tip reached it, or it was thrown out.
     /\ (tipIdx >= i \/ failed[i])
     \* The thread may work on it.
     /\ MayWork(a, i)
     \* The line moves past it.
     /\ dto' = i
     \* The pass names the order whose turn is now near (step 2).
     /\ nearTo' = Named(i)
     \* Nothing else changes.
     /\ UNCHANGED <<fly, unitVars, wokenTo, nearMark, owes, ringVars, decVars, ap, wr, failed, tipVars, epochVars, durable, pub, visible,
                    claimVars, failVars, threadVars>>

\* Order i's record may be written by thread a in one go, without the write
\* unit. Without Copies, a thread writes its own record and the record of
\* the next order to finish, and the round's holder, before it syncs,
\* writes every record the pipeline's state covers. With Copies only the
\* round's holder does that (write_unit.rs, Await::Copy: it waits for
\* nobody); every other thread goes through the write unit, and writes a
\* record alone only once its patience ran out (StealStart).
CanWrite(a, i) ==
  \* The order is a commit with a record, and the record is not written.
  /\ Brings(i) /\ ~wr[i]
  \* The epoch gate: the record was placed in the log's current epoch
  \* (mutant NoEpochGate: any epoch).
  /\ (ep[i] = logEp \/ Mutant = "NoEpochGate")
  \* The thread is still here.
  /\ Live(a)
  \* Either it holds the round of a healthy log, has not begun its sync,
  \* and the order is one the pipeline's state covers...
  /\ \/ a \in round /\ target[a] = 0 /\ ~poisoned /\ rbLast = 0 /\ i <= Covered
     \* ...or, without Copies, the order is settled, the thread may work on
     \* it, and it is the thread's own or the next to finish.
     \/ ~Copies /\ i <= dto /\ MayWork(a, i) /\ (i = mine[a] \/ i = pub + 1)

\* Thread a writes order i's record into the log at its place, in one step.
Write(a, i) ==
  \* Writes are not taken in two steps in this run.
  /\ ~Copies
  \* The record may be written.
  /\ CanWrite(a, i)
  \* The record is in the log.
  /\ wr' = [wr EXCEPT ![i] = TRUE]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, threadVars>>

\* With copies, the first of a write's two steps, for the round's holder:
\* thread a counts in at the log's gate and looks again at whether order
\* i's record is written (WG4). It is not, so the write goes ahead: from
\* here its bytes are on their way to the host (write_unit.rs,
\* copy_record, through Log::write_unless). A run of the write unit may be
\* on its way beside it with the same record.
WriteStart(a, i) ==
  \* Writes take two steps in this run.
  /\ Copies
  \* The thread has no other write in flight.
  /\ fly[a] = {}
  \* The record may be written, and is not written yet (the look again).
  /\ CanWrite(a, i)
  \* Its write is in flight: one record.
  /\ fly' = [fly EXCEPT ![a] = {i}]
  \* Nothing else changes.
  /\ UNCHANGED <<writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub,
                 visible, claimVars, failVars, threadVars>>

\* The second step: thread a's write lands, a copy or a whole run at once,
\* and its records are in the log, whoever's write got there first. The
\* thread counts out and marks each record written, and everybody parked
\* on one of them is woken: the mark reads the parked flag in the same
\* instruction that sets the written one (descriptor.rs, D5; write_unit.rs,
\* try_batch and copy_record).
WriteLand(a) ==
  \* The records whose sleepers wake now: a copy's, marked written with a
  \* wake (Descriptor::mark). A run's are marked quietly, and its holder
  \* wakes their sleepers once it let the unit go (Release, WU7).
  LET now == IF writer = a THEN {} ELSE fly[a]
  \* The thread has a write in flight.
  IN /\ fly[a] # {}
     \* Its records are in the log.
     /\ wr'  = [j \in Slots |-> wr[j] \/ j \in fly[a]]
     \* The write is over.
     /\ fly' = [fly EXCEPT ![a] = {}]
     \* Whoever slept on one of the records woken now is awake...
     /\ asleep' = {b \in asleep : sleepOn[b] \notin now}
     \* ...and waits for nothing now.
     /\ sleepOn' = [b \in Actors |-> IF b \in asleep /\ sleepOn[b] \in now THEN 0 ELSE sleepOn[b]]
  \* Nothing else changes.
     /\ UNCHANGED <<moreVars, filled, writer, gathered, late, wanted, ringVars, decVars, ap, failed, dto,
                    tipVars, epochVars, durable, pub, visible, claimVars, failVars, threadVars>>

\* With copies, a write in flight fails: the device says no. The log is
\* poisoned, its records are not in the log, and whoever slept on one of
\* them wakes to find the log failed (write_unit.rs: the holder or the
\* copier poisons the log and wakes the parked).
LandFail(a) ==
  \* The thread has a write in flight.
  /\ fly[a] # {}
  \* The run still has a failure to spend.
  /\ fails < MaxFails
  \* The log is poisoned.
  /\ poisoned' = TRUE
  \* One more failure happened.
  /\ fails'    = fails + 1
  \* The write is over, and wrote nothing.
  /\ fly'      = [fly EXCEPT ![a] = {}]
  \* Whoever slept on one of its records is awake...
  /\ asleep'   = {b \in asleep : sleepOn[b] \notin fly[a]}
  \* ...and waits for nothing now.
  /\ sleepOn'  = [b \in Actors |-> IF b \in asleep /\ sleepOn[b] \in fly[a] THEN 0 ELSE sleepOn[b]]
  \* Nothing else changes.
  /\ UNCHANGED <<moreVars, filled, writer, gathered, late, wanted, ringVars, decVars, ap, wr, failed, dto,
                 tipVars, epochVars, durable, pub, visible, claimVars, rbLast, frozen, threadVars>>

\* THE WRITE UNIT (write_unit.rs). With Copies, the records reach the log
\* the way the code sends them: each owner encodes its own record, or a
\* thread that needs one nobody is encoding encodes it (EncodeStart,
\* EncodeEnd);
\* one thread at a time holds the write unit, gathers the run of filled
\* records that waits first (Gather), counts in at the gate and looks at
\* them again (RunStart), writes them with one write (WriteLand), and lets
\* the unit go (Release). A thread whose record is in another's hands parks
\* on it (Park) until somebody wakes it, or until its patience runs out
\* (Timeout): then it writes the record itself, as a copy (StealStart).
\*
\* Tiny example: threads 1 and 2 each have a settled order. Thread 1 fills
\* its record and claims the unit; thread 2 fills its record too and finds
\* the unit held, so it parks on its record, asking to be woken when the
\* unit is let go. Thread 1's run held only record 1 (record 2 was not
\* filled when it looked), so when thread 1 lets the unit go it wakes thread
\* 2, which claims the unit and writes record 2. Had thread 1 stopped while
\* holding the unit, thread 2 would have run out of patience and written
\* record 2 itself.

\* Thread a claims the encoding of order i's record: its own, once i is
\* settled, or the record it needs next when nobody is encoding it
\* (write_unit.rs, RegolithEngine::fill; descriptor.rs, Descriptor::fill,
\* the FILLING claim, one read-modify-write). Mutant EncodeNoClaim skips the
\* claim, so two threads can encode one record into one buffer at once.
EncodeStart(a, i) ==
  \* A run that shows writes step by step, by a thread that helps.
  /\ Copies /\ Helping(a)
  \* Its own record, or the one it needs next.
  /\ i \in {mine[a], Need(a)} \ {0}
  \* Settled, a commit with a record...
  /\ i <= dto /\ Brings(i)
  \* ...not filled, not in the log, not thrown out...
  /\ ~filled[i] /\ ~wr[i] /\ ~failed[i]
  \* ...and nobody is encoding it (the broken version does not look).
  /\ (~filling[i] \/ Mutant = "EncodeNoClaim")
  \* The claim is this thread's...
  /\ filling'  = [filling EXCEPT ![i] = TRUE]
  \* ...and it has the record in hand.
  /\ encoding' = [encoding EXCEPT ![a] = i]
  \* Nothing else changes.
  /\ UNCHANGED <<wakeVars, noteVars, fly, filled, writer, gathered, asleep, sleepOn, late, wanted, runEnd, turnAsleep, turnAt,
                 ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, threadVars>>

\* Thread a finishes encoding the record it has in hand and sets the filled
\* flag. Whoever slept waiting for it to be filled is woken: setting the
\* flag reads the parked flag in the same instruction (D5).
EncodeEnd(a) ==
  \* The record the thread is encoding.
  LET i == encoding[a]
  \* It has one in hand.
  IN /\ i # 0
     \* The record is filled.
     /\ filled'   = [filled EXCEPT ![i] = TRUE]
     \* The thread has nothing in hand now.
     /\ encoding' = [encoding EXCEPT ![a] = 0]
     \* Whoever slept on it is awake...
     /\ asleep'   = {b \in asleep : sleepOn[b] # i}
     \* ...and waits for nothing now.
     /\ sleepOn'  = [b \in Actors |-> IF b \in asleep /\ sleepOn[b] = i THEN 0 ELSE sleepOn[b]]
     \* Nothing else changes.
     /\ UNCHANGED <<wakeVars, noteVars, fly, writer, gathered, late, wanted, filling, runEnd, turnAsleep, turnAt, ringVars,
                    decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible, claimVars,
                    failVars, threadVars>>

\* Thread a claims the free write unit with one compare-and-swap and
\* gathers the first run: the first usable record past the published
\* frontier and every usable record after it (write_unit.rs, try_batch).
\* It encodes nothing: it only collects records their owners filled (WU2).
\* A thread claims the unit when a record it needs is filled and not
\* written; the round's holder claims it before its sync, for the records
\* the sync covers.
Gather(a) ==
  \* How far the run may look: the round's holder covers everything the
  \* pipeline's state covers; any other thread the settled orders.
  LET top == IF a \in round THEN Covered ELSE dto
  \* In a run that shows writes step by step, the unit is free.
  IN /\ Copies /\ writer = 0
     \* The thread needs a record written: a filled one of its own needs...
     /\ \/ Helping(a) /\ Need(a) # 0 /\ filled[Need(a)]
        \* ...or it holds the round of a healthy log and has not begun its
        \* sync.
        \/ a \in round /\ target[a] = 0 /\ ~poisoned /\ rbLast = 0
     \* Some record is usable.
     /\ Firsts(top) # {}
     \* The run starts at the first usable record.
     /\ gathered' = [gathered EXCEPT ![a] = RunFrom(Min(Firsts(top)), top)]
     \* The unit is this thread's.
     /\ writer'   = a
     \* A new claim, told from every earlier one.
     /\ claimNo'  = claimNo + 1
     \* Nothing else changes.
     /\ UNCHANGED <<filling, encoding, runEnd, turnAsleep, turnAt, wakeVars, saw, noted, looking, fly, filled,
                    asleep, sleepOn, late, wanted, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars,
                    durable, pub, visible, claimVars, failVars, threadVars>>

\* The holder counts its run in at the log's gate, and looks at the records
\* again: a record another thread wrote since the gather ends the run
\* before it (write_unit.rs, WU3; Log::count_in). The gate lets the run in
\* only in the epoch its records were placed in; after a rollback it is
\* turned away and writes nothing (WG1). What is kept is in flight.
RunStart(a) ==
  \* The thread holds the unit, with a run gathered and nothing in flight.
  /\ writer = a /\ gathered[a] # {} /\ fly[a] = {}
  \* In flight: what the look keeps, if the gate lets the run in.
  /\ LET kept == IF ep[Min(gathered[a])] = logEp THEN Kept(gathered[a]) ELSE {}
     \* In flight: what the look keeps, if the gate lets the run in...
     IN /\ fly' = [fly EXCEPT ![a] = kept]
        \* ...and the holder will publish up to its last record (WU7).
        /\ runEnd' = [runEnd EXCEPT ![a] = IF kept = {} THEN runEnd[a] ELSE Max(kept)]
  \* The run is no longer only gathered.
  /\ gathered' = [gathered EXCEPT ![a] = {}]
  \* Nothing else changes.
  /\ UNCHANGED <<wakeVars, noteVars, filled, writer, asleep, sleepOn, late, wanted, filling, encoding, turnAsleep, turnAt,
                 ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, threadVars>>

\* The holder lets the unit go, its run landed (write_unit.rs, Claim's drop
\* and tell_waiters). The swap that lets it go reads whether a parked thread
\* noted the claim; if one did, every record still waiting for the unit has
\* its sleepers woken (WriteUnit::tell, wake_if_waiting). Then the run's
\* own sleepers are woken (WU7). Mutant LostWake wakes nobody that waits;
\* mutant FirstOnly wakes those on the first waiting record only and puts
\* the note back on the unit, now free, for a next holder that may never
\* come, as the code once did.
Release(a) ==
  \* The parked threads whose record is filled and still waits.
  LET waiting  == {b \in asleep : sleepOn[b] # 0 /\ filled[sleepOn[b]] /\ ~wr[sleepOn[b]]
                                   /\ ~failed[sleepOn[b]]}
      \* The first record among theirs.
      firstRec == Min({sleepOn[b] : b \in waiting})
      \* Who is told: every waiting thread, if somebody noted the claim.
      told     == IF wanted /\ waiting # {} /\ Mutant # "LostWake"
                    \* The broken version: those on the first record only.
                    THEN IF Mutant = "FirstOnly" THEN {b \in waiting : sleepOn[b] = firstRec}
                         \* The real code: all of them.
                         ELSE waiting
                    \* Nobody noted it, nobody waits, or the broken version: nobody.
                    ELSE {}
      \* The run's own sleepers: their records are written now.
      ran      == {b \in asleep : sleepOn[b] # 0 /\ wr[sleepOn[b]]}
  \* The thread holds the unit, with nothing gathered and nothing in flight.
  IN /\ writer = a /\ gathered[a] = {} /\ fly[a] = {}
     \* The unit is free.
     /\ writer'  = 0
     \* The told and the run's sleepers are awake...
     /\ asleep'  = asleep \ (told \cup ran)
     \* ...and wait for nothing now.
     /\ sleepOn' = [b \in Actors |-> IF b \in told \cup ran THEN 0 ELSE sleepOn[b]]
     \* The note is read and gone (the broken version puts it back while
     \* another record still has somebody parked on it).
     /\ wanted'  = (Mutant = "FirstOnly" /\ wanted /\ (waiting \ told) # {})
     \* Nothing else changes.
     /\ UNCHANGED <<moreVars, fly, filled, gathered, late, ringVars, decVars, ap, wr, failed, dto, tipVars,
                    epochVars, durable, pub, visible, claimVars, failVars, threadVars>>

\* Thread a finds the record it needs in another's hands and sets out to
\* park on it (write_unit.rs, bring_written then park_on): the record is
\* filled and the unit taken, and it remembers the claim it saw (`seen`);
\* or another thread is encoding it. The spin before the park is not
\* shown: a wait that ends by itself changes nothing. Between this look
\* and the park the holder may let the unit go, and others may claim it.
ParkLook(a) ==
  \* The record it needs.
  LET i == Need(a)
  \* A run that shows writes step by step; a thread that helps, holds
  \* neither the round nor a unit (those never park: Await::Copy).
  IN /\ Copies /\ Helping(a) /\ a \notin round /\ claimant # a
     \* It needs a record...
     /\ i # 0
     \* ...that is not its own while it waits for a sync (CopyOwn).
     /\ ~(a \in Immediate /\ i = mine[a])
     \* The record is filled and the unit is another thread's...
     /\ \/ filled[i] /\ writer # 0 /\ writer # a
        \* (...or another thread is encoding it.)
        \/ ~filled[i] /\ filling[i]
     \* It is about to park on it...
     /\ looking' = [looking EXCEPT ![a] = i]
     \* ...remembering the claim it saw, for a filled record.
     /\ saw'     = [saw EXCEPT ![a] = IF filled[i] THEN claimNo ELSE 0]
     \* Nothing else changes.
     /\ UNCHANGED <<filling, encoding, runEnd, turnAsleep, turnAt, wakeVars, claimNo, noted, fly, filled,
                    writer, gathered, asleep, sleepOn, late, wanted, ringVars, decVars, ap, wr, failed, dto,
                    tipVars, epochVars, durable, pub, visible, claimVars, failVars, threadVars>>

\* Thread a parks (descriptor.rs, Descriptor::park): it goes on the
\* record's list with the parked flag set, then looks again. For a filled
\* record its look puts a note on the claim it saw, with a compare-and-swap
\* that succeeds only while that very claim still holds the unit
\* (WriteUnit::note); a refused note means the claim is gone, and the
\* thread does not sleep: it looks at the record afresh. For a record being
\* encoded it sleeps unless it is filled by now. Mutant NoteTrustsALoad
\* trusts a load that may show the claim it saw, with somebody's note on
\* it, after that claim was let go: it sleeps on a note nobody reads.
ParkSleep(a) ==
  \* The record it set out to park on.
  LET i  == looking[a]
      \* The note lands: the claim it saw still holds the unit.
      lands == writer # 0 /\ claimNo = saw[a]
      \* Whether it sleeps: the record is not in the log or thrown out...
      sleeps == /\ ~wr[i] /\ ~failed[i]
                \* ...and, for a filled record, its note landed (or the
                \* broken version read an old note)...
                /\ IF saw[a] # 0
                     \* (A filled record: the note landed, or the broken
                     \* version read an old one.)
                     THEN lands \/ (Mutant = "NoteTrustsALoad" /\ saw[a] \in noted)
                     \* ...or the record is still being encoded.
                     ELSE ~filled[i]
  \* It set out to park.
  IN /\ i # 0
     \* It is no longer about to park.
     /\ looking' = [looking EXCEPT ![a] = 0]
     \* It sleeps, or looks afresh...
     /\ asleep'  = IF sleeps THEN asleep \cup {a} ELSE asleep
     \* ...on that record.
     /\ sleepOn' = IF sleeps THEN [sleepOn EXCEPT ![a] = i] ELSE sleepOn
     \* The note it put on the claim, if it landed...
     /\ wanted'  = (wanted \/ (saw[a] # 0 /\ lands))
     \* ...is remembered on that claim for good.
     /\ noted'   = IF saw[a] # 0 /\ lands THEN noted \cup {claimNo} ELSE noted
     \* Nothing else changes.
     /\ UNCHANGED <<filling, encoding, runEnd, turnAsleep, turnAt, wakeVars, claimNo, saw, fly, filled, writer,
                    gathered, late, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub,
                    visible, claimVars, failVars, threadVars>>

\* Thread a waits for a sync, and finds its own record in another thread's
\* hands: it does not sleep on it, it copies it, as the round's holder
\* does, and stays awake to claim the next round (pipeline.rs, drive with
\* Await::Copy for a commit that waits for a sync). The copy is
\* StealStart's: counted in, looked at again, written.
CopyOwn(a) ==
  \* The record it needs.
  LET i == Need(a)
  \* A run that shows writes step by step; a thread that helps, waits for a
  \* sync, and holds neither the round nor a unit.
  IN /\ Copies /\ Helping(a) /\ a \in Immediate /\ a \notin round /\ claimant # a
     \* It needs its own record.
     /\ i # 0 /\ i = mine[a]
     \* The record is filled and the unit is another thread's...
     /\ \/ filled[i] /\ writer # 0 /\ writer # a
        \* ...or another thread is encoding it.
        \/ ~filled[i] /\ filling[i]
     \* It is about to write it itself.
     /\ late' = [late EXCEPT ![a] = i]
     \* Nothing else changes.
     /\ UNCHANGED <<moreVars, fly, filled, writer, gathered, asleep, sleepOn, wanted, ringVars, decVars, ap, wr,
                    failed, dto, tipVars, epochVars, durable, pub, visible, claimVars, failVars, threadVars>>

\* STEP 2, A THREAD FAR FROM ITS TURN (pipeline.rs, P6). A blocking
\* thread finds the next order being decided by another thread, and its
\* own order is more than Lead orders past that one: instead of waiting on
\* that thread over and over, it parks on its own order (TurnPark). It is
\* woken once the decided frontier comes within Lead orders of its own
\* (TurnWake: the thread that passes the frontier names it, and a thread
\* whose commit is done wakes it), or when its patience runs out; then it
\* helps. It parks at most once while the frontier stands still.
\* Tiny example: thread 1 decides order 1; thread 3, whose order is 3,
\* parks; thread 1 passes order 1, which names order 3 when Lead is 1;
\* thread 3 wakes and decides order 2 and its own.

\* Thread a parks far from its turn.
TurnPark(a) ==
  \* The next order to decide.
  LET next == dto + 1
  \* A blocking thread that helps, holds neither the round nor a unit.
  IN /\ a \in Committers /\ Helping(a) /\ a \notin round /\ claimant # a
     \* Its own order is more than Lead past the next one...
     /\ mine[a] > next + Lead
     \* ...not named, not told: its turn is not near (the look after it
     \* registered on its own order; pipeline.rs, park_far's near).
     /\ mine[a] > nearTo /\ ~nearMark[mine[a]]
     \* Another thread is deciding the next order: it has noted the tip for
     \* it (the DECIDING claim).
     /\ \E b \in Actors \ {a} : seen[b] # NoNote /\ seen[b][1] = next
     \* It has not parked already while the frontier stood here.
     /\ turnAt[a] # next
     \* It sleeps...
     /\ turnAsleep' = turnAsleep \cup {a}
     \* ...remembering where the frontier stood.
     /\ turnAt'     = [turnAt EXCEPT ![a] = next]
     \* Nothing else changes.
     /\ UNCHANGED <<wakeVars, noteVars, fly, filled, writer, gathered, asleep, sleepOn, late, wanted, filling, encoding, runEnd,
                    ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                    claimVars, failVars, threadVars>>

\* Thread a, parked far from its turn, wakes: its order was told its turn
\* is near, or its patience ran out (TURN_PATIENCE). In the code a record
\* filled, written or thrown out wakes it too; the model leaves those wakes
\* out, so it shows that the ones it keeps are enough.
TurnWake(a) ==
  \* It sleeps far from its turn.
  /\ a \in turnAsleep
  \* It wakes when its order was marked near; otherwise only if this run
  \* allows patience, and then at any moment.
  /\ IF nearMark[mine[a]] THEN TRUE ELSE Patience
  \* It is awake and helps again.
  /\ turnAsleep' = turnAsleep \ {a}
  \* Nothing else changes.
  /\ UNCHANGED <<wakeVars, noteVars, fly, filled, writer, gathered, asleep, sleepOn, late, wanted, filling, encoding, runEnd,
                 turnAt, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub,
                 visible, claimVars, failVars, threadVars>>

\* Thread a, its commit done, tells the orders the frontier named since the
\* last telling that their turn is near (pipeline.rs, wake_near: one
\* compare-and-swap of woken_to up to near_to, then a NEAR mark on every
\* order in between that is in the ring, which wakes whoever parked on it).
\* Shown as one step, though in the code the claim of the range and the
\* marks are separate. That is safe: a mark only adds NEAR, so marks done
\* late, twice or in another order change nothing, and a thread that stops
\* between the claim and its marks leaves those orders to their patience,
\* which this model gives them only in the rows with Patience on. Mutant
\* TurnNeverTold claims the range and marks nothing.
WakeNear(a) ==
  \* The thread is here, awake, with its commit done.
  /\ owes[a] /\ Live(a) /\ a \notin asleep /\ a \notin turnAsleep
  \* The orders it tells: past the last told, up to the last named, and in
  \* the ring.
  /\ LET told == {i \in Slots : i > wokenTo /\ i <= nearTo /\ cell[i] = "ready"}
     \* Those orders are marked near (the broken version marks none).
     IN nearMark' = [i \in Slots |-> nearMark[i] \/ (i \in told /\ Mutant # "TurnNeverTold")]
  \* The range is told.
  /\ wokenTo' = IF nearTo > wokenTo THEN nearTo ELSE wokenTo
  \* The thread owes nothing now.
  /\ owes' = [owes EXCEPT ![a] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<nearTo, fly, unitVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable,
                 pub, visible, claimVars, failVars, threadVars>>

\* Thread a's patience runs out while it sleeps (write_unit.rs,
\* out_of_patience: PATIENCE, or the decided frontier moving on while the
\* claim it saw stays put, or the ring full). It wakes by itself, and will
\* write the record it waited for. Only in a run with Patience.
Timeout(a) ==
  \* Patience is on, and the thread sleeps.
  /\ Patience /\ a \in asleep
  \* It is awake...
  /\ asleep'  = asleep \ {a}
  \* ...about to write the record it waited for...
  /\ late'    = [late EXCEPT ![a] = sleepOn[a]]
  \* ...and waits for nothing now.
  /\ sleepOn' = [sleepOn EXCEPT ![a] = 0]
  \* Nothing else changes.
  /\ UNCHANGED <<moreVars, fly, filled, writer, gathered, wanted, ringVars, decVars, ap, wr, failed, dto, tipVars,
                 epochVars, durable, pub, visible, claimVars, failVars, threadVars>>

\* THE FAST PATH (write_unit.rs, write_alone, WU8). An owner whose record
\* is settled, and nobody else has in hand, finds the write unit free with
\* no request on it: it encodes its record and
\* writes it by itself, through the gate as a copy is (the count in and
\* the look again are StealStart's), skipping the unit's claim, gather and
\* wakes. Under contention the unit takes over. Mutant DirectAnyTime takes
\* the fast path whatever the unit says: the guard is about
\* speed, and the model stays safe without it (row Green_Direct_AnyTime).
\* Mutant DirectNoRecheck writes without the look again: see StealStart.
DirectStart(a) ==
  \* Its own order.
  LET i == mine[a]
  \* A run that shows writes step by step; a thread that helps, holds
  \* neither the round nor a unit.
  IN /\ Copies /\ Helping(a) /\ a \notin round /\ claimant # a
     \* Settled, a commit with a record...
     /\ i <= dto /\ Brings(i)
     \* ...that nobody has in hand, not in the log, not thrown out.
     /\ ~filled[i] /\ ~filling[i] /\ ~wr[i] /\ ~failed[i]
     \* The unit is free with no request on it (the broken version does not
     \* look).
     /\ \/ writer = 0 /\ ~wanted
        \* (Or the broken version.)
        \/ Mutant = "DirectAnyTime"
     \* The record is encoded...
     /\ filled'  = [filled EXCEPT ![i] = TRUE]
     \* ...which wakes whoever slept on it...
     /\ asleep'  = {b \in asleep : sleepOn[b] # i}
     \* ...and they wait for nothing now.
     /\ sleepOn' = [b \in Actors |-> IF b \in asleep /\ sleepOn[b] = i THEN 0 ELSE sleepOn[b]]
     \* The thread is about to write it by itself.
     /\ late'    = [late EXCEPT ![a] = i]
     \* Nothing else changes.
     /\ UNCHANGED <<moreVars, fly, writer, gathered, wanted, ringVars, decVars, ap, wr, failed, dto, tipVars,
                    epochVars, durable, pub, visible, claimVars, failVars, threadVars>>

\* The thread whose patience ran out writes its record itself, as a copy
\* (write_unit.rs, copy_record through Log::write_unless): it counts in at
\* the gate, in the epoch the record was placed in, and looks again whether
\* the record is written or thrown out; only if neither is the write in
\* flight (WG4). Mutant StealNoRecheck writes without looking again, so a
\* record a sync already covers can be written over it.
StealStart(a) ==
  \* The record it gave up waiting for.
  LET i == late[a]
  \* The thread ran out of patience, and has no write in flight.
  IN /\ i # 0 /\ fly[a] = {}
     \* In flight: the record, if the gate lets it in (its epoch is the
     \* log's) and the look again finds it neither written nor thrown out.
     \* The two broken versions skip that look: StealNoRecheck for every
     \* copy, DirectNoRecheck for an owner's own record.
     /\ fly' = [fly EXCEPT ![a] = IF ep[i] = logEp /\ ((~wr[i] /\ ~failed[i]) \/ Mutant = "StealNoRecheck"
                                                    \/ (Mutant = "DirectNoRecheck" /\ i = mine[a]))
                                   \* Let in: the record is in flight.
                                   THEN {i}
                                   \* Turned away, or found written: nothing.
                                   ELSE {}]
     \* It is no longer about to write.
     /\ late' = [late EXCEPT ![a] = 0]
     \* Nothing else changes.
     /\ UNCHANGED <<moreVars, filled, writer, gathered, asleep, sleepOn, wanted, ringVars, decVars, ap, wr, failed,
                    dto, tipVars, epochVars, durable, pub, visible, claimVars, failVars, threadVars>>

\* Thread a writes order i's record and the device says no: the log is
\* poisoned, and nothing may vouch for it until it is rolled back. (With
\* Copies a write fails as it lands: LandFail.)
WriteFail(a, i) ==
  \* Writes take one step in this run.
  /\ ~Copies
  \* The record was about to be written.
  /\ CanWrite(a, i)
  \* The run still has a failure to spend.
  /\ fails < MaxFails
  \* The log is poisoned.
  /\ poisoned' = TRUE
  \* One more failure happened.
  /\ fails'    = fails + 1
  \* Nothing else changes: the record is not in the log.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, rbLast, frozen, threadVars>>

\* Thread a has a reason to claim the round: the next order to finish is a
\* commit that cannot be finished as it stands, because it waits for a
\* sync or because the log failed; or the thread claimed a unit, the
\* pipeline is drained, and the log failed (the unit rolls it back first).
WantsRound(a) ==
  \* The order after the published frontier.
  LET p == pub + 1
  \* It is settled, and the thread may work on it...
  IN \/ /\ p <= dto /\ MayWork(a, p)
        \* ...it is a commit that is not publishable...
        /\ dec[p] = "commit" /\ ~Publishable(p)
        \* ...and it is written (so it waits for a sync) or the log failed.
        /\ (wr[p] \/ poisoned)
     \* Or: the thread's unit waits for a failed log to be rolled back.
     \/ claimant = a /\ Live(a) /\ poisoned /\ pub = dto

\* Thread a claims the round: it will do the log's device step. One
\* compare-and-swap, so one holder at a time (mutant TwoSyncs: a load and a
\* store, so two).
ClaimRound(a) ==
  \* The thread has a reason to.
  /\ WantsRound(a)
  \* Nobody holds the round (the broken version does not check).
  /\ (round = {} \/ Mutant = "TwoSyncs")
  \* This thread does not hold it already.
  /\ a \notin round
  \* Now it does.
  /\ round' = round \cup {a}
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 target, claimant, failVars, threadVars>>

\* The round's holder starts the sync: it works out how far the log is
\* written with no gap, and will vouch for exactly that.
SyncBegin(a) ==
  \* The thread holds the round and has no sync under way.
  /\ a \in round /\ target[a] = 0
  \* The log is healthy.
  /\ ~poisoned
  \* There is something to make safe.
  /\ SyncTarget > durable
  \* No write still in flight is of a record it will vouch for: it waited
  \* for those to land (WG4). The broken version, SyncNoWait, does not
  \* wait: its first branch lets the sync begin whatever is in flight.
  /\ \/ Mutant = "SyncNoWait"
     \* The real code: every write in flight is past what it vouches for.
     \/ \A b \in Actors : \A j \in fly[b] : sq[j] > SyncTarget
  \* It notes how far it will vouch.
  /\ target' = [target EXCEPT ![a] = SyncTarget]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 round, claimant, failVars, threadVars>>

\* The sync returns: what the holder noted is safe now. The round is free
\* again, unless the log failed meanwhile: then the holder rolls back next.
SyncOk(a) ==
  \* The thread has a sync under way.
  /\ a \in round /\ target[a] # 0
  \* The durable mark moves to what the sync vouched for.
  /\ durable' = target[a]
  \* The sync is over.
  /\ target'  = [target EXCEPT ![a] = 0]
  \* The round is given up, or kept for the rollback.
  /\ round'   = IF poisoned THEN round ELSE round \ {a}
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, pub, visible, claimant,
                 failVars, threadVars>>

\* The sync returns and the device says no. The log is poisoned; the
\* holder keeps the round and rolls back next.
SyncFail(a) ==
  \* The thread has a sync under way.
  /\ a \in round /\ target[a] # 0
  \* The run still has a failure to spend.
  /\ fails < MaxFails
  \* The log is poisoned.
  /\ poisoned' = TRUE
  \* One more failure happened.
  /\ fails'    = fails + 1
  \* The sync is over, and vouched for nothing.
  /\ target'   = [target EXCEPT ![a] = 0]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 round, claimant, rbLast, frozen, threadVars>>

\* The round's holder finds nothing to do and lets the round go. In the
\* design this never happens (a round is claimed only for work, and only
\* its holder can do that work); it is here for the mutant with two
\* holders, where one finds the other did it.
RoundIdle(a) ==
  \* The thread holds the round with no sync under way...
  /\ a \in round /\ target[a] = 0
  \* ...the log is healthy and nothing waits to be made safe...
  /\ ~poisoned /\ SyncTarget <= durable
  \* ...and no rollback is running.
  /\ rbLast = 0
  \* The round is given up.
  /\ round' = round \ {a}
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 target, claimant, failVars, threadVars>>

\* The rollback's first step, by the round's holder: both frontiers stop
\* (mutant NoFreeze: they do not), and the pipeline moves to a new epoch,
\* so every verdict worked out from the old state is recognisable as stale.
\* The holder notes the last ticket the old state reached.
RbFreeze(a) ==
  \* The thread holds the round with no sync under way.
  /\ a \in round /\ target[a] = 0
  \* The log failed.
  /\ poisoned
  \* No rollback has started.
  /\ rbLast = 0
  \* The frontiers stop.
  /\ frozen' = (Mutant # "NoFreeze")
  \* A new epoch begins.
  /\ epoch'  = epoch + 1
  \* The last ticket of the old state, kept as one more than it.
  /\ rbLast' = Covered + 1
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, logEp, memGen, gen, durable, pub,
                 visible, claimVars, poisoned, fails, threadVars>>

\* The rollback's second step: finish what can be finished, throw out the
\* rest, cut their records from the log and their writes from the
\* memtable, hand their sequences back, tell the owners, and let the
\* pipeline go on.
RbFinish(a) ==
  \* The last ticket of the old state.
  LET last == rbLast - 1
      \* (Only a broken version lets the frontier get ahead of it.)
      hi   == IF pub > last THEN pub ELSE last
      \* How far the orders can be finished as they are.
      upto == CHOOSE p \in pub..hi :
                \* Every order up to p may be finished, and the next may not.
                (\A j \in (pub + 1)..p : Publishable(j)) /\ (p = hi \/ ~Publishable(p + 1))
      \* The orders to throw out: every order past that point (mutant
      \* RollbackForgets throws none out).
      out  == IF Mutant = "RollbackForgets" THEN {}
              \* The real code: every order in the ring past that point.
              ELSE {j \in (upto + 1)..last : cell[j] = "ready"}
      \* Is the memtable replaced? Always (mutant KeepMemtable: only when
      \* an order being thrown out had written to it).
      swap == Mutant # "KeepMemtable" \/ \E j \in out : InMem(j)
  \* The thread holds the round...
  IN /\ a \in round
     \* ...and its rollback is under way.
     /\ rbLast # 0
     \* The writes in flight have landed: it cuts the log only after (WG2).
     /\ \A b \in Actors : fly[b] = {}
     \* The publishable orders are finished here: readers see them.
     /\ visible' = Max({visible, SeqUpTo(upto)})
     \* The rest are thrown out.
     /\ failed'  = [j \in Slots |-> failed[j] \/ j \in out]
     \* Readers are shown a new memtable from now on...
     /\ memGen'  = IF swap THEN memGen + 1 ELSE memGen
     \* ...into which the rebuild copied the orders up to that point and
     \* nothing past it.
     /\ memOf'   = [j \in Slots |-> IF swap /\ j <= upto /\ InMem(j) THEN memGen + 1 ELSE memOf[j]]
     \* The log is cut back to that point...
     /\ wr'      = [j \in Slots |-> wr[j] /\ j <= upto]
     \* ...and turns away every record placed before now.
     /\ logEp'   = epoch
     \* The tip is the state after the last finished order...
     /\ tipIdx'  = last
     \* ...so the sequences of the thrown-out orders are handed out again.
     /\ tipNext' = IF Mutant = "RollbackForgets" THEN tipNext ELSE SeqUpTo(upto)
     \* The decided frontier jumps past everything the rollback dealt with...
     /\ dto'     = hi
     \* ...and so does the published one.
     /\ pub'     = hi
     \* Every owner the frontier passed is told: "failed" if thrown out.
     /\ pc'      = [b \in Actors |->
                      IF pc[b] = "ready" /\ Live(b) /\ mine[b] > pub /\ mine[b] <= hi
                        THEN ToldAs(failed[mine[b]] \/ mine[b] \in out)
                        ELSE pc[b]]
     \* The frontiers move again.
     /\ frozen'  = FALSE
     \* The log is healthy again.
     /\ poisoned' = FALSE
     \* The rollback is over.
     /\ rbLast'  = 0
     \* The round is given up.
     /\ round'   = round \ {a}
     \* Whoever slept on a record thrown out, or for a call that returns
     \* here, is awake: marking a slot failed wakes the parked (D5)...
     /\ asleep' = {b \in asleep : sleepOn[b] \notin out /\ ~(mine[b] > pub /\ mine[b] <= hi)}
     \* ...and waits for nothing now.
     /\ sleepOn' = [b \in Actors |->
                      IF b \in asleep /\ (sleepOn[b] \in out \/ (mine[b] > pub /\ mine[b] <= hi))
                        THEN 0 ELSE sleepOn[b]]
     \* Nothing else changes.
     \* A committer told here will tell the orders whose turn is near.
     /\ owes'    = [b \in Actors |->
                      owes[b] \/ (b \in Committers /\ pc[b] = "ready" /\ Live(b) /\ mine[b] > pub
                                 /\ mine[b] <= hi)]
     \* An owner asleep far from its turn wakes when its order is thrown out
     \* (FAILED wakes the slot's sleepers) or lands here (Descriptor::land).
     /\ turnAsleep' = {b \in turnAsleep : mine[b] \notin out /\ ~(mine[b] > pub /\ mine[b] <= hi)}
     \* The jump names the orders whose turn is now near, as a pass would.
     /\ nearTo'  = Named(hi)
     \* Nothing else changes.
     /\ UNCHANGED <<filling, encoding, runEnd, turnAt, noteVars, wokenTo, nearMark, fly, filled,
                    writer, gathered, late, wanted, ringVars, dec, sq, ep, logOf, ap, tipUsed, epoch, gen,
                    durable, target, claimant, fails, snap, hor, mine, seen, stopped>>

\* Thread a raises the read horizon to the next order's sequence: the
\* first half of publishing it.
PubHorizon(a) ==
  \* The order after the published frontier.
  LET p == pub + 1
  \* It is decided and settled.
  IN /\ p <= dto
     \* The steps are taken one by one.
     /\ SmallSteps
     \* No rollback holds the frontier.
     /\ ~frozen
     \* It may be finished (mutant RunSkipsCheck: a holder publishing its
     \* run takes a written record as finished).
     /\ (Publishable(p) \/ (Mutant = "RunSkipsCheck" /\ PublishesRun(a, p) /\ wr[p]))
     \* It holds a sequence readers do not see yet.
     /\ Takes(p) /\ visible < sq[p]
     \* The thread may work on it, or its last run wrote that far (WU7).
     /\ (MayWork(a, p) \/ PublishesRun(a, p))
     \* Readers now see it.
     /\ visible' = sq[p]
     \* Nothing else changes.
     /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub,
                    claimVars, failVars, threadVars>>

\* Thread a moves the published frontier past the next order: the second
\* half. The horizon is there already (mutant FrontierFirst does not wait
\* for it), so an owner who sees its order finished can read its own write.
\* The owner's call returns: it is told "done", or "failed" if a rollback
\* threw the order out. When the steps are taken in one go, this step
\* raises the horizon too.
PubFrontier(a) ==
  \* The order after the published frontier...
  LET p == pub + 1
      \* ...and who owns it.
      o == owner[p]
  \* It is decided and settled.
  IN /\ p <= dto
     \* No rollback holds the frontier.
     /\ ~frozen
     \* It may be finished (mutant RunSkipsCheck: see PubHorizon).
     /\ (Publishable(p) \/ (Mutant = "RunSkipsCheck" /\ PublishesRun(a, p) /\ wr[p]))
     \* Taken one by one: if it holds a sequence, readers see it already.
     /\ (SmallSteps /\ Takes(p) => visible >= sq[p] \/ Mutant = "FrontierFirst")
     \* The thread may work on it, or its last run wrote that far (WU7).
     /\ (MayWork(a, p) \/ PublishesRun(a, p))
     \* The order is finished.
     /\ pub' = p
     \* Taken in one go: the horizon is raised here.
     /\ visible' = IF ~SmallSteps /\ Takes(p) /\ sq[p] > visible THEN sq[p] ELSE visible
     \* Its owner, if it is waiting on this very order, is told.
     /\ pc' = IF o # 0 /\ pc[o] = "ready" /\ mine[o] = p /\ Live(o)
                \* It learns "done", or "failed" for an order thrown out.
                THEN [pc EXCEPT ![o] = ToldAs(failed[p])]
                \* Nobody is waiting on it here: no thread is told.
                ELSE pc
     \* A committer so told will tell the orders whose turn is near.
     /\ owes' = IF o \in Committers /\ pc[o] = "ready" /\ mine[o] = p /\ Live(o)
                \* It will, before it returns.
                THEN [owes EXCEPT ![o] = TRUE]
                \* Nobody told here.
                ELSE owes
     \* The order lands: an owner asleep far from its turn is woken by it
     \* (Descriptor::land, for a blocking slot that had a sleeper).
     /\ turnAsleep' = IF o # 0 THEN turnAsleep \ {o} ELSE turnAsleep
     \* Nothing else changes.
     /\ UNCHANGED <<fly, filled, writer, gathered, asleep, sleepOn, late, wanted, filling, encoding, runEnd,
                    turnAt, noteVars, nearTo, wokenTo, nearMark, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable,
                    claimVars, failVars, snap, hor, mine, seen, stopped>>

\* Mutant OutOfOrder only: the horizon jumps to a later order that happens
\* to be ready, over an earlier one that is not.
PublishAny(a, i) ==
  \* Only the broken version has this step.
  /\ Mutant = "OutOfOrder"
  \* An order beyond the next one...
  /\ i > pub + 1 /\ i <= dto
  \* ...that commits and could be finished on its own...
  /\ dec[i] = "commit" /\ Takes(i) /\ Publishable(i)
  \* ...which the thread may work on...
  /\ MayWork(a, i)
  \* ...and readers do not see yet.
  /\ visible < sq[i]
  \* The horizon jumps to it.
  /\ visible' = sq[i]
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub,
                 claimVars, failVars, threadVars>>

\* Thread a claims the job at the next ticket: an ordered unit, or the
\* rotation check of a write that finds the memtable full. One
\* compare-and-swap; the thread that asked for the unit is only the
\* likeliest to win it.
ClaimNext(a) ==
  \* The next ticket holds a unit, or a write whose turn needs the check.
  /\ (NextIsUnit \/ NextIsTurn)
  \* No rollback holds the frontier.
  /\ ~frozen
  \* Nobody claimed it.
  /\ claimant = 0
  \* The thread may work on that ticket.
  /\ MayWork(a, dto + 1)
  \* The job is this thread's.
  /\ claimant' = a
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 round, target, failVars, threadVars>>

\* The claimant can run its job: everything before the ticket is finished,
\* nobody is on the log's device step, and the log is healthy (a failed log
\* is rolled back first).
CanRunNext(a) ==
  \* The job is this thread's, and the thread is still here.
  /\ claimant = a /\ Live(a)
  \* No rollback is running and the log is healthy.
  /\ ~frozen /\ ~poisoned
  \* Nobody holds the round: the job holds it for its own length.
  /\ round = {}
  \* No write is in flight: the job syncs the log, and waited for those
  \* first (WG4).
  /\ \A b \in Actors : fly[b] = {}
  \* The pipeline is drained up to the ticket.
  /\ Drained(dto + 1)

\* The claimant runs the ordered unit at the next ticket: an ingest's
\* install, say. It syncs the log, takes a sequence if it brings keys,
\* installs its table, and its verdict becomes the tip, all with the round
\* held, so the model takes it as one step.
UnitRun(a) ==
  \* The next ticket in line.
  LET i == dto + 1
  \* It holds a unit not yet run.
  IN /\ NextIsUnit
     \* The claimant may run it now.
     /\ CanRunNext(a)
     \* The unit ran.
     /\ dec'     = [dec EXCEPT ![i] = "unit"]
     \* It takes the next sequence if it brings keys (an ingested table
     \* reads at it); a unit that brings none takes none.
     /\ sq'      = [sq EXCEPT ![i] = IF Writes[owner[i]] # {} THEN tipNext + 1 ELSE 0]
     \* Its verdict is of the current state...
     /\ ep'      = [ep EXCEPT ![i] = epoch]
     \* ...of the current log...
     /\ logOf'   = [logOf EXCEPT ![i] = gen]
     \* ...and of the current memtable.
     /\ memOf'   = [memOf EXCEPT ![i] = memGen]
     \* Its table is installed: readers at its sequence find it.
     /\ ap'      = [ap EXCEPT ![i] = TRUE]
     \* It synced the log first: everything decided before it is safe.
     /\ durable' = IF tipNext > durable THEN tipNext ELSE durable
     \* The tip is the state the unit left...
     /\ tipIdx'  = i
     \* ...with its sequence, if any, the last handed out.
     /\ tipNext' = IF Writes[owner[i]] # {} THEN tipNext + 1 ELSE tipNext
     \* The line moves past it.
     /\ dto'     = i
     \* The pass names the order whose turn is now near (step 2).
     /\ nearTo'  = Named(i)
     \* The job is done.
     /\ claimant' = 0
     \* Nothing else changes.
     /\ UNCHANGED <<fly, unitVars, wokenTo, nearMark, owes, ringVars, wr, failed, tipUsed, epochVars, pub, visible, round, target, failVars,
                    threadVars>>

\* The claimant runs the rotation check in the turn of the write at the
\* next ticket: the full memtable is sealed behind a fresh one and a fresh
\* log, after the old log is synced to its end. The state it leaves takes
\* the tip's place, and the write is then decided from it.
TurnRun(a) ==
  \* The next ticket holds a write that finds the memtable full.
  /\ NextIsTurn
  \* The claimant may run the check now.
  /\ CanRunNext(a)
  \* The old log is synced to its end before the next one exists.
  /\ durable' = IF tipNext > durable THEN tipNext ELSE durable
  \* Commits after this go to the next log...
  /\ gen'     = gen + 1
  \* ...and to an empty memtable.
  /\ tipUsed' = 0
  \* The job is done.
  /\ claimant' = 0
  \* Nothing else changes: the write itself is decided next.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipIdx, tipNext, epoch, logEp, memGen, pub,
                 visible, round, target, failVars, threadVars>>

\* One of the pipeline's steps, by a thread that is in none of the write
\* unit's waits or claims.
PipelineStep(a) ==
  \* A transaction beginning early:
  /\ \/ Begin(a) \/ EarlyAbort(a)
     \* Getting into the ring:
     \/ Reserve(a) \/ Fill(a) \/ Retry(a) \/ Void(a)
     \* Deciding and settling the next order:
     \/ ReadTip(a) \/ DecideCas(a) \/ DecideNow(a) \/ TipMove(a) \/ MarkStale(a) \/ Pass(a)
     \* The round: a sync...
     \/ ClaimRound(a) \/ SyncBegin(a) \/ SyncOk(a) \/ SyncFail(a) \/ RoundIdle(a)
     \* ...or a rollback. And publishing:
     \/ RbFreeze(a) \/ RbFinish(a) \/ PubHorizon(a) \/ PubFrontier(a)
     \* An ordered unit or a rotation check:
     \/ ClaimNext(a) \/ UnitRun(a) \/ TurnRun(a)
     \* The steps that name an order: applying it, writing its record.
     \/ \E i \in Slots : Apply(a, i) \/ Write(a, i) \/ WriteFail(a, i) \/ PublishAny(a, i)
     \* With copies, a write in two steps: counted in for an order, then landed.
     \/ \E i \in Slots : WriteStart(a, i)
     \* The write unit: a record's encoding claimed, a run gathered, a park.
     \/ \E i \in Slots : EncodeStart(a, i)
     \* (A run gathered, a record found in another's hands.)
     \/ Gather(a) \/ ParkLook(a)
     \* The fast path: an owner writes its record by itself.
     \/ DirectStart(a)
     \* A thread that waits for a sync copies its record rather than sleep.
     \/ CopyOwn(a)
     \* Step 2: a park far from the thread's turn.
     \/ TurnPark(a)

\* Thread a is in none of the write unit's waits or claims: it holds no
\* unit, does not sleep, has not run out of patience, and has no write in
\* flight. Only such a thread takes the pipeline's other steps; one that is
\* in the middle of a write takes only that write's steps, as the code,
\* which does each of them in one call, does.
Free(a) == writer # a /\ a \notin asleep /\ late[a] = 0 /\ fly[a] = {} /\ encoding[a] = 0
           \* (Nor asleep far from its turn, nor about to park.)
           /\ a \notin turnAsleep /\ looking[a] = 0

\* Any one step of a thread that has not walked away.
LiveStep(a) ==
  \* The thread is still here...
  /\ Live(a)
  \* ...and, in the middle of a write: the write's next step; the run's
  \* count in, its landing (or failure) and the unit let go; the time out
  \* of a park, and the copy after it.
  /\ \/ RunStart(a) \/ WriteLand(a) \/ LandFail(a) \/ Release(a) \/ Timeout(a) \/ StealStart(a)
     \* (An encoding finished, a park far from the turn ended, the orders
     \* whose turn is near told by a thread whose commit is done.)
     \/ EncodeEnd(a) \/ TurnWake(a) \/ WakeNear(a)
     \* (A park under way: the note and the look again.)
     \/ ParkSleep(a)
     \* ...or, in none of those, one of the pipeline's other steps.
     \/ Free(a) /\ PipelineStep(a)

\* Thread a walks away for good, anywhere in its commit: descheduled
\* forever, or its task dropped. It never does so while it holds the round
\* or a claimed unit: see WHAT IS NOT A STOPPED THREAD (mutant
\* StopHoldsClaim allows it).
Stop(a) ==
  \* The run still allows another thread to stop.
  /\ Cardinality(stopped) < MaxStops
  \* This thread is one of those that may...
  /\ a \in Stoppers
  \* ...and has not stopped yet.
  /\ a \notin stopped
  \* It is somewhere inside its commit.
  /\ pc[a] \in {"begun", "claimed", "ready"}
  \* It holds neither the round nor a claimed unit.
  /\ ((a \notin round /\ claimant # a) \/ Mutant = "StopHoldsClaim")
  \* It has no write in flight: those bytes are with the host, which
  \* finishes the write whatever the thread does next.
  /\ fly[a] = {}
  \* It is gone.
  /\ stopped' = stopped \cup {a}
  \* Nothing else changes.
  /\ UNCHANGED <<fly, writeVars, ringVars, decVars, ap, wr, failed, dto, tipVars, epochVars, durable, pub, visible,
                 claimVars, failVars, pc, snap, hor, mine, seen>>

\* A step of some thread that is still here.
LiveNext == \E a \in Actors : LiveStep(a)

\* Every step the system can take: a live thread's step, or a thread
\* walking away.
Next == LiveNext \/ \E a \in Actors : Stop(a)

\* Every behaviour: start at Init, take steps, and as long as some live
\* thread can take a step, one of them eventually does. No single thread is
\* promised a turn: the scheduler may starve any one of them, which is the
\* scheduler lock-freedom is defined against.
Spec == Init /\ [][Next]_vars /\ WF_vars(LiveNext)

====
