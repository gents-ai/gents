---- MODULE CommitOrderedAppend ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A log is a numbered list that only grows: entry 1, entry 2, entry 3, and
\* so on. Many transactions want to add entries at the same time, and a
\* reader follows the log with a cursor ("I have read up to 2, give me what
\* comes after"). For that to work the numbers must have no holes, no number
\* may be used twice, and a smaller number must mean "committed earlier".
\*
\* `Transaction::append(log, entry, once_key)` (src/transaction/append.rs)
\* does not pick the number when it is called. The transaction only queues
\* the entry. The number is given out at the very end, in the one step of a
\* commit that runs for one transaction at a time, in commit order ("the
\* ordered step": AppendOrder::number, src/engine/commit/append.rs), and only
\* after the commit's reads were checked.
\*
\* Tiny example. Writers A and B each append one entry at the same moment,
\* and the log's head says 0. If each read the head (0) and wrote "my entry
\* is number 1", there would be two entries numbered 1 and one of them would
\* overwrite the other. In the ordered step A is numbered first and gets 1;
\* B counts from the head A just left and gets 2, even though A's entry is
\* not yet on disk. If A's commit had been refused instead, it would have
\* taken no number, and B would get 1: no hole.
\*
\* What can go wrong, and each is a RED configuration below: two entries with
\* one number; a hole left by a refused or failed commit; numbers out of
\* commit order; an append that makes two unrelated transactions conflict;
\* a reader whose cursor moves past an entry it never saw; the same message
\* appended twice although it carried a "once key".
\*
\* What this model shows: over every interleaving of a few writers, groups
\* of commits, refused commits, write-ahead-log errors and reads, none of
\* those happens. It carries the five invariants and four REDs of an
\* earlier model of the same log under generic names, and adds density,
\* at-most-once keys, numbering at commit, commits decided before the ones
\* ahead of them are published, aborts, a failed group, the Phase 7
\* pipeline and the transaction's own view. Crashes per durability mode are
\* in CommitOrderedAppendCrash.tla.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/Append.lean:
\*   reachable_inv              the ordered step keeps the invariant below,
\*                              whatever it decides and whoever aborts
\*   dense_from_one             positions assigned after validation are
\*                              exactly 1..head, in assignment order
\*   unique_positions           no two rows share a position
\*   at_most_once               no two rows carry one once key
\*   commit_order               a smaller position belongs to an earlier
\*                              commit, or to an earlier append of the same one
\*   snapshot_sees_prefix       a snapshot whose head is H sees exactly the
\*                              rows at positions 1..H, all of them
\*   view_only_head_duplicates  the RED ViewOnlyHead, as a counterexample
\*   assign_before_validation_leaves_hole
\*                              the RED AssignBeforeValidation, likewise
\*   stamps_leave_hole          the RED Stamps, likewise
\* TLC checks the same invariants here over every interleaving of a few
\* writers, groups, aborts, WAL errors and reads.
\*
\* THE DESIGN (plan 3.6 and 4.7), and the code that implements it.
\*   Transaction::append      queues (log, entry, once key) in the
\*     transaction as a PendingAppend (src/transaction/append.rs). Nothing is
\*     read or validated for it, and the transaction never sees its own
\*     appends: their positions do not exist until commit.
\*   The ordered step         every commit takes a slot of the commit ring,
\*     and the slots are decided in ring order by whichever thread reaches
\*     them (RegolithEngine::decide_work, src/engine/commit/pipeline.rs).
\*     Deciding a commit first validates its reads
\*     (RegolithEngine::validate, src/engine/commit/txn.rs); only a commit
\*     that passes gets a sequence and positions (AppendOrder::number,
\*     src/engine/commit/append.rs). The head and the once keys are read
\*     from the view, which holds every commit decided before this one (a
\*     slot is in the memtable before the next is decided): nothing is
\*     cached, so commits a failure discards leave no trace.
\*     A GROUP, in this model, is a run of commits decided one after
\*     another in one step, before any of them is written and published:
\*     neighbouring slots of the ring. Each member reads what the earlier
\*     members left, as each slot reads what the earlier slots left. The
\*     engine has no leader and forms no group; the word is the model's.
\*   Positions                the next position is head + 1. An append whose
\*     once key already holds a position appends nothing. Each assigned
\*     append writes entry_key(p) = entry, once_key = p and head_key = p in
\*     the member's own atomic commit.
\*   Publication              the group's writes become visible together,
\*     at its sequences. A reader bounds itself by the head key in its own
\*     snapshot and reads positions (cursor, head]; a missing row there is a
\*     failed read, never a skip.
\*   The Phase 7 pipeline (4.7, Depth = 2)   the decide stage runs ahead of
\*     publication: a later group decides while an earlier decided group is
\*     still being written. It reads the decided prefix: the view plus every
\*     decided, unpublished group. (That pipeline is modelled on its own in
\*     CommitPipeline.tla.)
\*
\* THE REJECTED DESIGNS AND MISUSES. Each is a RED configuration.
\*   Mode = "Counter"          the head key is read and
\*     bumped inside the transaction, as an ordinary validated write. Two
\*     appenders that touch nothing else in common conflict on it.
\*   Mode = "Detached"         the transaction writes an unnumbered
\*     pending row; a second,
\*     separate commit later numbers every pending row in begin order. A
\*     second durable commit per write, positions out of commit order, and
\*     an append committed without its number.
\*   Mode = "Stamp"            (regolith #216, #218; plan D1) the position
\*     is the commit's sequence number. Every commit takes a sequence, so a
\*     commit that does not append leaves a hole in the log.
\*   Mutant = "LatestBound"    the reader bounds itself
\*     by the latest assigned position instead of the head in its snapshot,
\*     moving its cursor past rows that are decided but not yet visible.
\*   Mutant = "ViewOnlyHead"   a group member reads the head from the view
\*     only, ignoring the earlier members of its group.
\*   Mutant = "OnceFromView"   a once key is looked up in the view only,
\*     ignoring earlier members of the group and earlier appends of the
\*     same transaction.
\*   Mutant = "AssignBeforeValidation"   positions are taken before the
\*     member is validated, so a member that then aborts consumes them.
\*   Mutant = "HeadCache"      the head lives in a cache advanced at
\*     assignment, before publication; a WAL error does not roll it back.
\*   Mutant = "PublishedView"  (Phase 7) the decide stage reads the
\*     published view while an earlier decided group is unpublished.
\*
\* READINGS CHOSEN WHERE THE PLAN LEAVES ROOM.
\*   - A validation failure on another key is a nondeterministic outcome
\*     for the writers in MayAbort: whatever the other key is, an append
\*     must stay correct, so any failure pattern is allowed.
\*   - A WAL error fails the group at the front of the pipeline and every
\*     group decided after it (they were decided on top of it). With
\*     Depth = 2 the plan latches the engine read-only after such an error
\*     (4.7 item 7); this model lets the engine continue, which allows more
\*     behaviours, so every invariant that holds here holds there too.
\*   - After a group, the head key holds the last position any committed
\*     member of it assigned (each assignment writes head_key = p); a group
\*     that assigns nothing leaves the head key alone.
\*   - Groups publish in decide order (publication is over the contiguous
\*     decided prefix), and a WAL error is injected at the front group only:
\*     an error at a later group is the same as publishing the earlier ones
\*     first, and readers see only published state.
\*   - Stamps (Mode = "Stamp") take one sequence per commit; that red uses
\*     writers that append at most once and no once keys, which is the shape
\*     the earlier model checked.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - One log. Logs have disjoint head, entry and once keys, so two logs
\*     are two copies of this model that share only the commit order.
\*   - Entries are named by <<writer, append index>>. A row is the fact
\*     "this append was assigned this position". In the store a second
\*     assignment of one position overwrites the entry_key put of the first;
\*     keeping both as facts lets INV_UniqueCursors name the defect directly.
\*   - A reader takes its snapshot and reads in one step, at the published
\*     view. Without crashes the view only grows, so an older snapshot is
\*     an earlier published view, on which every invariant was checked.
\*   - Group order is any order of the chosen members, which includes the
\*     arrival order the engine uses.
\*   - A transaction whose commit aborted or failed does not retry; a retry
\*     is a new transaction, which another writer already plays.
\*
\* CONFIGURATIONS. Teeth (plan 7.1 item 11) come two ways. Each RED lists
\* the invariants it is not about before the one it names, so the state TLC
\* reports breaks the named one alone. And each mutant has a Teeth
\* configuration, GREEN, that checks every invariant but the ones it is
\* about over its whole state space. Invariants a mutant breaks by design
\* are left out of both and named in its configuration.
\*   MC_CommitOrderedAppend_Green_Writers      every writer shape, one at a time
\*   MC_CommitOrderedAppend_Green_Groups       groups of two
\*   MC_CommitOrderedAppend_Green_Faults       groups of two with aborts
\*                                             and a WAL error
\*   MC_CommitOrderedAppend_Green_Pipeline     two decided groups in flight
\*   MC_CommitOrderedAppend_Teeth_<mutant>     GREEN, one per mutant below
\*                                             (Detached covers its three REDs)
\*   MC_CommitOrderedAppend_Red_Counter                 INV_NoDisjointConflict
\*   MC_CommitOrderedAppend_Red_Detached                INV_OneCommitPerWrite
\*   MC_CommitOrderedAppend_Red_DetachedOrder           INV_CommitOrder
\*   MC_CommitOrderedAppend_Red_DetachedNumbering       INV_NumberedAtCommit
\*   MC_CommitOrderedAppend_Red_LatestBound             INV_NoSkip
\*   MC_CommitOrderedAppend_Red_Stamps                  INV_Dense
\*   MC_CommitOrderedAppend_Red_ViewOnlyHead            INV_UniqueCursors
\*   MC_CommitOrderedAppend_Red_OnceFromView            INV_AtMostOnce
\*   MC_CommitOrderedAppend_Red_AssignBeforeValidation  INV_Dense
\*   MC_CommitOrderedAppend_Red_HeadCache               INV_Dense
\*   MC_CommitOrderedAppend_Red_PublishedView           INV_UniqueCursors

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration (an MC_*.cfg file) chooses. A writer is one transaction; the
\* sets below say what each one appends.
CONSTANTS
  \* The writers that append one entry and give no once key.
  Singles,
  \* The writers that append one entry with the once key "o1". They all share that key, so at most
  \* one of them may get a row.
  SharedOnce,
  \* The writers that append two entries to the log, with no once key.
  Doubles,
  \* The writers that append twice with the same once key "o2": only the first of the two may get a
  \* row.
  RepeatOnce,
  \* The writers whose commit appends nothing at all.
  Silent,
  \* The writers whose commit may be refused because of some other key they read (their validation
  \* may fail).
  MayAbort,
  \* The largest number of commits decided in one step, one after another: a group. The engine
  \* decides one slot at a time and has no such bound; the bound keeps the model small.
  GroupSize,
  \* How many decided groups may be waiting to be written and published at once. In the engine
  \* any number of decided slots may wait (the commit ring, src/engine/commit/ring.rs); 1 and 2
  \* are the two depths checked here.
  Depth,
  \* The largest number of write-ahead-log errors the model injects.
  MaxWalErrors,
  \* How positions are given out: "Append" is the design the engine implements; "Counter",
  \* "Detached" and "Stamp" are the rejected designs.
  Mode,
  \* "none", or one misuse of the design, named in the header.
  Mutant

\* Every writer. Each writer is one transaction, which commits at most once.
Writers == Singles \cup SharedOnce \cup Doubles \cup RepeatOnce \cup Silent

\* The marker for an append made without a once key.
NoOnce == "none"

\* The once keys any writer uses.
OnceKeys == {"o1", "o2"}

\* The appends writer w makes, in the order it calls `append`: each element
\* is the once key passed, or NoOnce.
Appends(w) ==
  \* A single: one append with no once key.
  CASE w \in Singles    -> <<NoOnce>>
    \* A shared-once writer: one append with the once key "o1".
    [] w \in SharedOnce -> <<"o1">>
    \* A double: two appends with no once key.
    [] w \in Doubles    -> <<NoOnce, NoOnce>>
    \* A repeat-once writer: two appends, both with the once key "o2".
    [] w \in RepeatOnce -> <<"o2", "o2">>
    \* A silent writer: no append.
    [] w \in Silent     -> <<>>

\* The writer shapes are disjoint, so every writer has exactly one shape.
ASSUME \A A, B \in {Singles, SharedOnce, Doubles, RepeatOnce, Silent} :
         \* (two different shapes share no writer)
         A # B => A \cap B = {}
\* Only writers can abort; a group holds at least one member; the pipeline
\* is one or two groups deep; the mode and mutant are ones named above.
\* Only a writer can be one that may abort.
ASSUME MayAbort \subseteq Writers
\* A group has at least one member, the pipeline is one or two deep, and the error budget is a
\* number.
ASSUME GroupSize \in Nat \ {0} /\ Depth \in {1, 2} /\ MaxWalErrors \in Nat
\* The mode is one of the four named above.
ASSUME Mode \in {"Append", "Counter", "Detached", "Stamp"}
\* The mutant is "none" or one of the six named above.
ASSUME Mutant \in {"none", "LatestBound", "ViewOnlyHead", "OnceFromView",
                   "AssignBeforeValidation", "HeadCache", "PublishedView"}
\* A mutant is a misuse of the design, so it runs on the design.
ASSUME Mutant # "none" => Mode = "Append"
\* The rejected designs ran one commit at a time, with the earlier model's
\* writer shapes:
\* stamps on writers appending at most once, no once keys where the design
\* had none.
ASSUME Mode # "Append" => Depth = 1
\* Stamps were checked on writers that append at most once, with no once key.
ASSUME Mode = "Stamp" => Doubles \cup RepeatOnce \cup SharedOnce = {}
\* The detached design had no once keys.
ASSUME Mode = "Detached" => RepeatOnce \cup SharedOnce = {}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* Where each transaction is in its life (Phases below); a Transaction of src/transaction.rs.
  phase,
  \* Counter design only: the head each transaction read in its snapshot. The engine has no such
  \* thing: its head key is never part of a read set.
  snapHead,
  \* Detached design only: the order in which the transactions began.
  beginNo,
  \* Detached design only: the next begin-order number to hand out.
  nextBeginNo,
  \* The sequence number each writer's commit took (the engine's commit sequence).
  cseq,
  \* The log as readers see it: one record [pos, w, i, once] per stored entry, meaning "append i of
  \* writer w sits at position pos". In the engine each is the put entry_key(pos) = entry
  \* (LogLayout::entry_key, src/log_layout.rs).
  rows,
  \* The position each once key holds in the published view, 0 when it holds none. In the engine the
  \* once key stores the position as 8 bytes.
  onceAt,
  \* The published head key: the newest position given out, 0 for an empty log
  \* (LogLayout::head_key).
  head,
  \* The newest commit sequence readers may see (RegolithEngine::visible_seq, a ReadHorizon,
  \* src/engine/read_horizon.rs).
  vseq,
  \* The groups already decided (validated and numbered) but not yet written and published, oldest
  \* first. In the engine these are the slots decided and not yet published (an AppendOrder holds
  \* one commit's running head, src/engine/commit/append.rs).
  inflight,
  \* Detached design only: the committed appends <<w, i>> that have no number yet.
  pending,
  \* HeadCache mutant only: a head kept in memory next to the store.
  cache,
  \* How many write-ahead-log errors were injected so far.
  walErrors,
  \* How many durable commits were issued, of any kind. Model bookkeeping for the
  \* one-commit-per-write check.
  commits,
  \* How many writer transactions committed. Model bookkeeping for the same check.
  writes,
  \* The reader's cursor: a consumer outside the database that has read every position up to it.
  rcur,
  \* The entries <<w, i>> that reader was handed so far.
  seen,
  \* TRUE once a read found no row at a position at or below the head of its own snapshot.
  readFailed

\* The variables the transactions own.
txnVars == <<phase, snapHead, beginNo, nextBeginNo, cseq>>
\* The variables the engine owns.
engineVars == <<rows, onceAt, head, vseq, inflight, pending, cache, walErrors, commits, writes>>
\* The variables the reader owns.
readerVars == <<rcur, seen, readFailed>>
\* Every variable, so a step that changes none of them is a stutter.
vars == <<txnVars, engineVars, readerVars>>

\* A transaction's phases: not begun, open (its body runs and it may read),
\* decided (validated and numbered, waiting for publication), done
\* (published: its commit returned), aborted (its validation failed), and
\* failed (its group hit a WAL error, so its commit returned an error).
Phases == {"idle", "open", "decided", "done", "aborted", "failed"}

\* The set of elements of a sequence.
Range(s) == {s[k] : k \in DOMAIN s}

----------------------------------------------------------------------------
\* The decided prefix: what the ordered step reads.

\* The newest decided group, the one the next decision builds on.
Last == inflight[Len(inflight)]

\* The head after every decided group: the published head when nothing is in
\* flight, else the head the newest decided group leaves.
DecidedHead == IF inflight = <<>> THEN head ELSE Last.postHead

\* The once keys after every decided group, likewise.
DecidedOnce == IF inflight = <<>> THEN onceAt ELSE Last.postOnce

\* The sequence after every decided group, likewise. Sequences are always
\* taken from the decided prefix; no mutant here is about them.
DecidedSeq == IF inflight = <<>> THEN vseq ELSE Last.postSeq

\* The head a new group starts from. The design: the decided prefix. The
\* HeadCache mutant: its cache. The PublishedView mutant: the published
\* view, missing any decided group still in flight.
StartHead ==
  \* The HeadCache mutant counts from its cache.
  CASE Mutant = "HeadCache"     -> cache
    \* The PublishedView mutant counts from the published head.
    [] Mutant = "PublishedView" -> head
    \* The design counts from the head after every decided group.
    [] OTHER                    -> DecidedHead

\* The once keys a new group starts from, by the same rule.
StartOnce == IF Mutant = "PublishedView" THEN onceAt ELSE DecidedOnce

----------------------------------------------------------------------------
\* The ordered step. It walks a group's members in group order, carrying a
\* running state: the head the next position is counted from, the last
\* position the group wrote to the head key (0 for none), the once keys
\* and the sequence as the earlier members left them, the rows and pending
\* appends the group adds, the members that committed and those that
\* aborted, and each writer's sequence.

\* Writer w's i-th append against the running state st. If its once key
\* already holds a position (in the view, an earlier member, or an earlier
\* append of w), it appends nothing. Otherwise it takes the next position:
\* head + 1, or, for stamps, the commit's own sequence; and it writes the
\* entry, the once key and head_key = p. OnceFromView looks the once key up
\* in the group's starting view only.
AssignOne(st, w, i) ==
  \* o: the once key of this append, or NoOnce.
  LET o   == Appends(w)[i]
      \* dup: the append has a once key,
      dup == /\ o # NoOnce
             \* and that key already holds a position (looked up only in the group's starting view
             \* under the mutant OnceFromView).
             /\ IF Mutant = "OnceFromView" THEN StartOnce[o] # 0 ELSE st.onceAt[o] # 0
      \* p: the position it would take: the commit's sequence under stamps, else one past the
      \* running head.
      p   == IF Mode = "Stamp" THEN st.seq ELSE st.head + 1
  \* A duplicate changes nothing.
  IN IF dup THEN st
     \* Otherwise: the running head moves to p (stamps keep no head),
     ELSE [st EXCEPT !.head    = IF Mode = "Stamp" THEN @ ELSE p,
                     \* p is the last value the group wrote to the head key,
                     !.headPut = IF Mode = "Stamp" THEN @ ELSE p,
                     \* the once key, if any, now holds p,
                     !.onceAt  = IF o = NoOnce THEN @ ELSE [@ EXCEPT ![o] = p],
                     \* and the row "append i of w sits at p" is added.
                     !.rows    = @ \cup {[pos |-> p, w |-> w, i |-> i, once |-> o]}]

\* Writer w's appends from the i-th on, in append order: several appends of
\* one transaction take consecutive positions.
RECURSIVE AssignFrom(_, _, _)
\* Number append i, then go on with append i + 1,
AssignFrom(st, w, i) ==
  \* until w has no append left.
  IF i > Len(Appends(w)) THEN st ELSE AssignFrom(AssignOne(st, w, i), w, i + 1)

\* One member w of the group, given the set F of members whose validation on
\* another key fails.
\*   valid: w is not in F, and, for Counter, the head it read in its
\*     snapshot is still the head (an ordinary validated read of it).
\*   start: w's sequence is the next one; ViewOnlyHead also resets the head
\*     to the group's starting head, forgetting earlier members.
\*   after: the running state once w's appends are numbered (Detached: once
\*     they are queued unnumbered).
\* A valid member commits with all of that. An invalid one aborts and takes
\* nothing; under AssignBeforeValidation it has already taken its positions,
\* so the running head keeps them (its own writes, the head key among them,
\* are discarded with it).
DecideMember(st, w, F) ==
  \* valid: its validation on another key did not fail,
  LET valid == /\ w \notin F
               \* and, under the Counter design, the head it read in its snapshot is still the head.
               /\ Mode = "Counter" => snapHead[w] = st.head
      \* start: under the mutant ViewOnlyHead the running head falls back to the group's starting
      \* head;
      start == [st EXCEPT !.head = IF Mutant = "ViewOnlyHead" THEN StartHead ELSE @,
                          \* and the member takes the next sequence.
                          !.seq  = @ + 1]
      \* after: under the Detached design,
      after == IF Mode = "Detached"
                 \* its appends are only queued, unnumbered;
                 THEN [start EXCEPT !.pend = @ \cup {<<w, i>> : i \in 1..Len(Appends(w))}]
                 \* otherwise they are numbered from the first on.
                 ELSE AssignFrom(start, w, 1)
  \* A valid member:
  IN CASE valid ->
            \* it joins the committed members and its sequence is recorded, with everything `after`
            \* holds.
            [after EXCEPT !.members = Append(@, w), !.cseq = [@ EXCEPT ![w] = start.seq]]
       \* An invalid member under the mutant AssignBeforeValidation:
       [] Mutant = "AssignBeforeValidation" ->
            \* it aborts, but the running head keeps the positions it took.
            [st EXCEPT !.head = after.head, !.aborted = @ \cup {w}]
       \* Any other invalid member:
       [] OTHER ->
            \* it aborts and takes nothing.
            [st EXCEPT !.aborted = @ \cup {w}]

\* Every member of group g in group order, starting from st.
RECURSIVE DecideAll(_, _, _)
\* Decide the first member, then the rest of the group,
DecideAll(st, g, F) ==
  \* until no member is left.
  IF g = <<>> THEN st ELSE DecideAll(DecideMember(st, Head(g), F), Tail(g), F)

\* The running state a group starts with: the starting head and once keys,
\* the decided sequence, nothing added or written yet.
GroupStart ==
  \* Its fields: the starting head, no head-key write yet, the starting once keys, the decided
  \* sequence,
  [head |-> StartHead, headPut |-> 0, onceAt |-> StartOnce, seq |-> DecidedSeq,
   \* no rows, no pending appends, no members, no aborts, and the sequences as recorded so far.
   rows |-> {}, pend |-> {}, members |-> <<>>, aborted |-> {}, cseq |-> cseq]

\* The groups that may be decided from the writers in S: one to GroupSize
\* distinct writers, in any order.
Groups(S) ==
  \* For each size n from 1 to GroupSize: every list of n different writers of S.
  UNION {{g \in [1..n -> S] : \A a, b \in 1..n : a # b => g[a] # g[b]} : n \in 1..GroupSize}

----------------------------------------------------------------------------
\* Actions.

\* The start: nobody has begun, the log is empty, nothing is in flight, and
\* the reader has read nothing.
Init ==
  \* No transaction has begun.
  /\ phase      = [w \in Writers |-> "idle"]
  \* No snapshot head was read (Counter).
  /\ snapHead   = [w \in Writers |-> 0]
  \* No begin order was handed out (Detached).
  /\ beginNo    = [w \in Writers |-> 0]
  \* The next begin-order number is 1.
  /\ nextBeginNo = 1
  \* No commit took a sequence.
  /\ cseq       = [w \in Writers |-> 0]
  \* The log holds no row.
  /\ rows       = {}
  \* No once key holds a position.
  /\ onceAt     = [o \in OnceKeys |-> 0]
  \* The head is 0: an empty log.
  /\ head       = 0
  \* No commit is visible.
  /\ vseq       = 0
  \* No group is decided and waiting.
  /\ inflight   = <<>>
  \* No append waits for a number (Detached).
  /\ pending    = {}
  \* The cached head is 0 (HeadCache).
  /\ cache      = 0
  \* No error was injected.
  /\ walErrors  = 0
  \* No durable commit was issued.
  /\ commits    = 0
  \* No writer committed.
  /\ writes     = 0
  \* The reader has read nothing.
  /\ rcur       = 0
  \* It was handed nothing.
  /\ seen       = {}
  \* No read has failed.
  /\ readFailed = FALSE

\* Writer w begins: its snapshot is the published view. Counter records the
\* head it reads there; Detached hands it the next begin-order number
\* (beginNo). Its appends are queued while it is open.
Begin(w) ==
  \* The writer has not begun.
  /\ phase[w] = "idle"
  \* It is open from now on.
  /\ phase'     = [phase EXCEPT ![w] = "open"]
  \* Under the Counter design it reads the head in its snapshot.
  /\ snapHead'  = IF Mode = "Counter" THEN [snapHead EXCEPT ![w] = head] ELSE snapHead
  \* Under the Detached design it gets the next begin-order number,
  /\ beginNo'   = IF Mode = "Detached" THEN [beginNo EXCEPT ![w] = nextBeginNo] ELSE beginNo
  \* and that number moves up.
  /\ nextBeginNo' = IF Mode = "Detached" THEN nextBeginNo + 1 ELSE nextBeginNo
  \* It has no commit sequence yet.
  /\ UNCHANGED <<cseq>>
  \* The engine's state does not change.
  /\ UNCHANGED engineVars
  \* The reader's state does not change.
  /\ UNCHANGED readerVars

\* The ordered step decides group g, in which the members in F fail
\* validation on another key. The group joins the pipeline as a record of
\* what it writes: its rows, the once keys it sets, its last head-key write
\* (0 for none), the head, once keys and sequence the decided prefix holds
\* after it (what the next decision reads), and its pending appends
\* (Detached). Members that committed are decided; the others aborted.
\* HeadCache advances its cache here, before publication.
Decide(g, F) ==
  \* The pipeline has room for one more decided group.
  /\ Len(inflight) < Depth
  \* st: the running state after every member of g was decided in order.
  /\ LET st == DecideAll(GroupStart, g, F)
     \* The group joins the pipeline as a record of:
     IN /\ inflight' = Append(inflight,
             \* the members that committed,
             [members  |-> st.members,
              \* the rows it adds,
              rows     |-> st.rows,
              \* the once keys it sets (0 for a key it leaves alone),
              onceSet  |-> [o \in OnceKeys |-> IF st.onceAt[o] # StartOnce[o] THEN st.onceAt[o] ELSE 0],
              \* its last write to the head key (0 for none),
              headPut  |-> st.headPut,
              \* the head the decided prefix holds after it,
              postHead |-> IF st.headPut # 0 THEN st.headPut ELSE DecidedHead,
              \* the once keys the decided prefix holds after it,
              postOnce |-> st.onceAt,
              \* the sequence the decided prefix holds after it,
              postSeq  |-> st.seq,
              \* and its pending appends (Detached).
              pend     |-> st.pend])
        \* Each writer's phase:
        /\ phase' = [w \in Writers |->
                       \* a member that committed is decided,
                       CASE w \in Range(st.members) -> "decided"
                         \* a member that failed validation is aborted,
                         [] w \in st.aborted        -> "aborted"
                         \* everyone else stays as they were.
                         [] OTHER                   -> phase[w]]
        \* The members' sequences are recorded.
        /\ cseq'  = st.cseq
        \* The HeadCache mutant moves its cache now, before anything is written.
        /\ cache' = IF Mutant = "HeadCache" THEN st.head ELSE cache
  \* Snapshot heads and begin orders do not change.
  /\ UNCHANGED <<snapHead, beginNo, nextBeginNo>>
  \* Nothing is published yet: the store, the counters and the error count stay.
  /\ UNCHANGED <<rows, onceAt, head, vseq, pending, walErrors, commits, writes>>
  \* The reader's state does not change.
  /\ UNCHANGED readerVars

\* The oldest decided group is written to the WAL, applied and published:
\* its rows, once keys and head become visible at its sequences, and its
\* members' commits return. Each member is one transaction's own commit.
Publish ==
  \* Some group is decided and waiting.
  /\ inflight # <<>>
  \* g: the oldest of them.
  /\ LET g == Head(inflight)
     \* Its rows become visible.
     IN /\ rows'     = rows \cup g.rows
        \* The once keys it set become visible.
        /\ onceAt'   = [o \in OnceKeys |-> IF g.onceSet[o] # 0 THEN g.onceSet[o] ELSE onceAt[o]]
        \* Its last head-key write becomes the head.
        /\ head'     = IF g.headPut # 0 THEN g.headPut ELSE head
        \* Its sequences become visible.
        /\ vseq'     = g.postSeq
        \* Its pending appends join the pending set (Detached).
        /\ pending'  = pending \cup g.pend
        \* Its members' commits return: they are done.
        /\ phase'    = [w \in Writers |-> IF w \in Range(g.members) THEN "done" ELSE phase[w]]
        \* One durable commit per member,
        /\ commits'  = commits + Len(g.members)
        \* and one committed writer per member.
        /\ writes'   = writes + Len(g.members)
        \* It leaves the pipeline.
        /\ inflight' = Tail(inflight)
  \* Snapshot heads, begin orders and sequences do not change.
  /\ UNCHANGED <<snapHead, beginNo, nextBeginNo, cseq>>
  \* The cache and the error count do not change.
  /\ UNCHANGED <<cache, walErrors>>
  \* The reader's state does not change.
  /\ UNCHANGED readerVars

\* A WAL error at the oldest decided group, after its positions were
\* assigned. That group and every group decided on top of it fail; their
\* members' commits return an error and nothing of them is published. The
\* next group decides from the view again. HeadCache's cache is not rolled
\* back.
WalError ==
  \* Some group is decided and waiting.
  /\ inflight # <<>>
  \* The error budget is not used up.
  /\ walErrors < MaxWalErrors
  \* failed: every member of every group in the pipeline.
  /\ LET failed == UNION {Range(inflight[k].members) : k \in 1..Len(inflight)}
     \* Their commits return an error.
     IN phase' = [w \in Writers |-> IF w \in failed THEN "failed" ELSE phase[w]]
  \* The pipeline is empty: nothing of those groups is published.
  /\ inflight'  = <<>>
  \* One more error was injected.
  /\ walErrors' = walErrors + 1
  \* Snapshot heads, begin orders and sequences do not change.
  /\ UNCHANGED <<snapHead, beginNo, nextBeginNo, cseq>>
  \* The store and the counters do not change; the HeadCache mutant's cache keeps its advance.
  /\ UNCHANGED <<rows, onceAt, head, vseq, pending, cache, commits, writes>>
  \* The reader's state does not change.
  /\ UNCHANGED readerVars

\* Detached only: the place of pending append e = <<w, i>> when the pending
\* appends are sorted by their writer's begin order, then append order;
\* 1 for the first. Begin order is the order of the numbers handed out at
\* begin.
Rank(e) ==
  \* Count the pending appends that sort at or before e: those of a writer that began earlier,
  Cardinality({x \in pending : \/ beginNo[x[1]] < beginNo[e[1]]
                               \* and those of the same writer made no later than e.
                               \/ x[1] = e[1] /\ x[2] <= e[2]})

\* Detached only: the second commit. It numbers every pending append
\* after the head by its rank, and writes the new head. It is a durable
\* commit of its own, with nothing else in flight, so it waits for nothing.
Sequence ==
  \* Only the Detached design has this step.
  /\ Mode = "Detached"
  \* Some append waits for a number.
  /\ pending # {}
  \* No group is in the pipeline.
  /\ inflight = <<>>
  \* Every pending append gets a row at head plus its rank, with no once key.
  /\ rows'    = rows \cup {[pos |-> head + Rank(e), w |-> e[1], i |-> e[2], once |-> NoOnce] : e \in pending}
  \* The head moves past all of them.
  /\ head'    = head + Cardinality(pending)
  \* Nothing is pending any more.
  /\ pending' = {}
  \* This commit takes a sequence of its own.
  /\ vseq'    = vseq + 1
  \* It is one more durable commit, and no writer's.
  /\ commits' = commits + 1
  \* The once keys, the pipeline, the cache, the error count and the writers' count do not change.
  /\ UNCHANGED <<onceAt, inflight, cache, walErrors, writes>>
  \* The transactions' state does not change.
  /\ UNCHANGED txnVars
  \* The reader's state does not change.
  /\ UNCHANGED readerVars

\* The head in the reader's snapshot, the published view: the head key, or,
\* for stamps, the published sequence.
SnapHead == IF Mode = "Stamp" THEN vseq ELSE head

\* The latest assigned position, decided or not: what LatestBound reads.
LatestHead == IF Mode = "Stamp" THEN DecidedSeq ELSE DecidedHead

\* The bound the reader reads up to. The design: the head in its snapshot.
Bound == IF Mutant = "LatestBound" THEN LatestHead ELSE SnapHead

\* Some position between the cursor and the snapshot's head holds no row.
Missing == \E p \in (rcur + 1)..SnapHead : \A r \in rows : r.pos # p

\* The reader reads positions (rcur, Bound] in its snapshot. A missing row
\* at or below its snapshot's head fails the read, and the cursor stays; a
\* reader never skips a row it can see is missing. Otherwise it is handed
\* every row in the range and moves its cursor to the bound. Positions
\* above the snapshot's head are not in its snapshot at all: only
\* LatestBound reads that far, and it cannot tell them from rows that are
\* not committed yet.
Read ==
  \* There is something to read past the cursor.
  /\ Bound > rcur
  \* No earlier read failed.
  /\ ~readFailed
  \* A position up to the snapshot's head has no row:
  /\ IF Missing
       \* the read fails,
       THEN /\ readFailed' = TRUE
            \* and the cursor and what was handed out stay.
            /\ UNCHANGED <<rcur, seen>>
       \* Otherwise the reader is handed every row above its cursor up to the bound,
       ELSE /\ seen' = seen \cup {<<r.w, r.i>> : r \in {x \in rows : rcur < x.pos /\ x.pos <= Bound}}
            \* its cursor moves to the bound,
            /\ rcur' = Bound
            \* and no failure is recorded.
            /\ UNCHANGED readFailed
  \* The transactions' state does not change.
  /\ UNCHANGED txnVars
  \* The engine's state does not change.
  /\ UNCHANGED engineVars

\* Every step the system can take. A group is formed from open writers,
\* and which of them fail validation on another key is picked.
Next ==
  \* A writer begins.
  \/ \E w \in Writers : Begin(w)
  \* A group g is formed from the open writers,
  \/ \E g \in Groups({w \in Writers : phase[w] = "open"}) :
       \* picks which members F fail validation on another key, and decides the group.
       \E F \in SUBSET (MayAbort \cap Range(g)) : Decide(g, F)
  \* The oldest decided group is written and published.
  \/ Publish
  \* A write-ahead-log error fails everything in the pipeline.
  \/ WalError
  \* The detached design's second commit numbers the pending appends.
  \/ Sequence
  \* The reader reads.
  \/ Read

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants. The first five are the earlier model's.

\* Every variable holds what its comment says.
TypeOK ==
  \* Every writer is in one of the six phases.
  /\ phase \in [Writers -> Phases]
  \* Every snapshot head is a number.
  /\ snapHead \in [Writers -> Nat]
  \* Every begin order is a number.
  /\ beginNo \in [Writers -> Nat]
  \* Every commit sequence is a number.
  /\ cseq \in [Writers -> Nat]
  \* Every row has a position from 1 up, a writer, and one of that writer's append indexes.
  /\ \A r \in rows : r.pos \in Nat \ {0} /\ r.w \in Writers /\ r.i \in 1..Len(Appends(r.w))
  \* Every once key holds a number.
  /\ onceAt \in [OnceKeys -> Nat]
  \* The head and the visible sequence are numbers.
  /\ head \in Nat /\ vseq \in Nat
  \* The pipeline is never deeper than Depth.
  /\ Len(inflight) <= Depth
  \* A pending append names a writer and its first or second append.
  /\ pending \subseteq Writers \X (1..2)
  \* No more errors than the budget.
  /\ walErrors <= MaxWalErrors
  \* The cursor is a number.
  /\ rcur \in Nat
  \* What the reader was handed names a writer and its first or second append.
  /\ seen \subseteq Writers \X (1..2)
  \* The failure mark is a yes or a no.
  /\ readFailed \in BOOLEAN

\* A position the reader has passed never gains a row it was not handed: a
\* row at or below the cursor was seen. Lean: snapshot_sees_prefix (the rows
\* at or below a snapshot's head never change).
\* Rules out, for example: the reader moves its cursor to 3 while row 2 is decided but not
\* published; row 2 appears later, below the cursor, and the reader never sees it. A consumer that
\* follows the log by cursor would silently lose an entry.
INV_NoSkip == \A r \in rows : r.pos <= rcur => <<r.w, r.i>> \in seen

\* No two appends were assigned one position. Lean: unique_positions.
\* Rules out, for example: two members of one group both count from the same head and both write
\* position 1. The second put would overwrite the first entry.
INV_UniqueCursors == \A a, b \in rows : a.pos = b.pos => a = b

\* Row a comes before row b in commit order: a's transaction took an
\* earlier sequence, or both are appends of one transaction and a was made
\* first.
\* Either a's transaction committed with a smaller sequence,
Before(a, b) == \/ cseq[a.w] < cseq[b.w]
                \* or both rows belong to one transaction and a was appended first.
                \/ a.w = b.w /\ a.i < b.i

\* Positions follow commit order, several appends per transaction included.
\* Lean: commit_order.
\* Rules out, for example: transaction A commits before transaction B, yet B's entry gets position 1
\* and A's gets position 2. Readers rely on positions to tell what came first.
INV_CommitOrder == \A a, b \in rows : a.pos < b.pos => Before(a, b)

\* An append never causes a conflict: the only writers that abort are those
\* whose validation on another key may fail. (In Lean, orderedStep takes
\* the validation outcome as given and reads nothing it validates.)
\* Rules out, for example: two transactions that only append to the same log, where one is refused
\* because both touched the head key. An append must never cost a retry.
INV_NoDisjointConflict == \A w \in Writers \ MayAbort : phase[w] # "aborted"

\* A write costs exactly its own commit: no second commit numbers it. (In
\* Lean, orderedStep numbers a transaction in the one step that commits it.)
\* Rules out, for example: a design where every append needs a second durable commit, later, just to
\* give it a number.
INV_OneCommitPerWrite == commits = writes

\* A snapshot with head H sees rows 1..H, every one of them, and no read
\* ever finds one missing. The view only grows here, so checking the
\* current view at every state checks every snapshot. Lean: dense_from_one
\* and snapshot_sees_prefix.
\* Rules out, for example: positions 1 and 3 hold rows and 2 holds none, because a refused commit or
\* a commit without appends used up a number. A reader walking 1, 2, 3 would hit a hole.
INV_Dense == {r.pos : r \in rows} = 1..SnapHead /\ ~readFailed

\* At most one row carries a once key, and the once key holds exactly that
\* row's position. Lean: at_most_once.
\* Rules out, for example: the same message is delivered twice, both deliveries append with the same
\* once key, and the log ends up holding two copies.
INV_AtMostOnce ==
  \* No two different rows carry the same once key.
  /\ \A a, b \in rows : a.once # NoOnce /\ a.once = b.once => a = b
  \* A row's once key holds exactly that row's position.
  /\ \A r \in rows : r.once # NoOnce => onceAt[r.once] = r.pos
  \* A once key that holds a position has a row carrying it.
  /\ \A o \in OnceKeys : onceAt[o] # 0 => \E r \in rows : r.once = o

\* Writer w's i-th append has its number in the view: its own row, or, for
\* an append whose once key was already taken, the position the once key
\* holds.
Numbered(w, i) ==
  \* Either the append has its own row,
  \/ \E r \in rows : r.w = w /\ r.i = i
  \* or it had a once key that already holds a position.
  \/ Appends(w)[i] # NoOnce /\ onceAt[Appends(w)[i]] # 0

\* Every append of a committed transaction has its position the moment the
\* commit returns. This replaces the earlier model's unchecked liveness ("eventually
\* numbered") with a safety property. (In Lean, by construction, as for
\* INV_OneCommitPerWrite.)
INV_NumberedAtCommit ==
  \* Rules out, for example: a commit returns "done" while its entry still has no position, so a
  \* caller that reads the log right after its own commit does not find its entry.
  \A w \in Writers : phase[w] = "done" => \A i \in 1..Len(Appends(w)) : Numbered(w, i)

\* While a transaction is open, and so may still read, none of its own
\* appends is in the view; its snapshot is an earlier view, so no read
\* inside it can see its own appends. It holds because positions exist only
\* after commit.
\* Rules out, for example: a transaction that scans the log before committing and finds its own, not
\* yet numbered, entry there.
INV_OwnAppendsInvisible == \A w \in Writers : phase[w] = "open" => \A r \in rows : r.w # w

====
