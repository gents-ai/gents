---- MODULE IngestPublication ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* Every write gets a sequence number from one shared counter: 1, 2, 3, ...
\* A reader takes a snapshot by noting "the newest sequence I may see" (the
\* read horizon) and from then on ignores everything newer. For that to be a
\* frozen picture, everything at or below the horizon must already be in
\* place when the reader notes it.
\*
\* An "ingest" adds a whole ready-made table of keys in one go. It draws one
\* sequence number for all its keys, and making the table part of the
\* database takes a while.
\*
\* Tiny example of what can go wrong. The ingest draws sequence 1 and starts
\* installing its table. A commit draws sequence 2, writes its key, and
\* raises the horizon to 2. A reader takes a snapshot at 2 and reads an
\* ingested key: absent, the table is not installed yet. Then the table
\* lands, with sequence 1, which is below the snapshot's 2. The same reader
\* reads the same key through the same snapshot again and now finds it. A
\* snapshot that changes is not a snapshot.
\*
\* The rule: the horizon never passes a sequence whose data is not installed
\* yet (a "pending slot"). The engine gets this by doing draw, install and
\* publish of an ingest under the same mutex every commit uses for its own
\* draw and publish, so nothing can publish in between.
\*
\* What this model shows: with that rule every snapshot reads every key the
\* same at every later moment, in every interleaving of commits, one ingest
\* and readers; and each of the two ways of breaking the rule breaks a
\* snapshot. (E8.)
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/Publication.lean:
\*   step_inv, reachable_inv        the visible sequence stays below every
\*                                  pending slot
\*   repeatable_snapshot            a snapshot reads every key the same at
\*                                  every later step
\*   early_publication_breaks_snapshots
\*                                  the RED case, as a counterexample
\*   fixed_protocol_refuses_early_publication
\*                                  the fix refuses that publication
\* TLC checks the same invariant here over every interleaving of a few
\* commits, one ingest and two readers.
\*
\* THE ENGINE.
\*   RegolithEngine::install_ordered    src/engine/ingest.rs
\*     In an ordered unit of the commit ring, takes the next sequence
\*     (`Ordered::take_seq`), installs the table in the version with a
\*     manifest edit recording that every entry of it reads at that sequence
\*     (D48), and only then publishes the sequence on the read horizon. A
\*     unit runs with every earlier commit published and no later one
\*     decided, so no commit takes a sequence or publishes in between.
\*   ReadHorizon::publish               src/engine/read_horizon.rs
\*     A `fetch_max`: the horizon only rises. A commit that drew a later
\*     sequence publishes it as soon as its own data is applied.
\*   RegolithEngine::register_snapshot_at_horizon   src/engine/mod.rs
\*     A snapshot reads at `ReadHorizon::visible`.
\*
\* THE DEFECT (E8, as the engine was before the fix). The ingest held the
\* compaction lock, not the write lock, so a commit could draw a later
\* sequence and publish it while the ingest's table was still being written.
\* The horizon then passed the ingest's sequence. A snapshot taken in that
\* window read the ingested keys as absent, and the same snapshot read them
\* as present once the table was installed. Two mutants show it, one per way
\* the horizon can pass the slot:
\*   CommitPassesSlot      a commit publishes past the pending ingest slot,
\*                         which is what the engine did before the fix;
\*   IngestPublishesEarly  the ingest publishes its own sequence when it
\*                         draws it, before its table is installed.
\*
\* THE FIX. The ingest's sequence is a pending slot from the moment it is
\* drawn until its table is installed, and the visible sequence never passes
\* a pending slot: a publication waits until no pending slot is at or below
\* the sequence it publishes. In the engine the wait is the ring's order
\* (THE ENGINE above): commits are published in ring order, and the
\* ingest's unit is one slot of the ring, from the moment it takes its
\* sequence to its own publication.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing. A read returns the
\* newest installed version at or below the snapshot. Over an ordered LSM
\* tree that is what the engine's read order returns (LsmOrder.tla, Lean
\* read_newest), so one set of installed versions stands for the memtables,
\* the levels and the ingested table.
\*
\* CONFIGURATIONS.
\*   MC_IngestPublication_Green                     Mutant = "none"
\*     RepeatableSnapshot and NoPendingAtOrBelowVisible hold.
\*   MC_IngestPublication_Red_CommitPassesSlot      RepeatableSnapshot fails.
\*   MC_IngestPublication_Red_IngestPublishesEarly  RepeatableSnapshot fails.

\* Borrow the standard TLA+ books: counting numbers and sizes of finite sets.
EXTENDS Naturals, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The commits, each named by a string. Each commit writes CommitKey once.
  Commits,
  \* The key every commit writes, a number.
  CommitKey,
  \* The keys the ingested table holds, as numbers.
  IngestKeys,
  \* The readers, each named by a string. Each takes one snapshot.
  Readers,
  \* "none" for the engine's rule, or the planted bug: "CommitPassesSlot" or "IngestPublishesEarly".
  Mutant

\* The ingest's id.
Ingest == "ingest"

\* The ingest's name is no commit's name.
ASSUME Ingest \notin Commits
\* Keys are numbers.
ASSUME CommitKey \in Nat /\ IngestKeys \subseteq Nat
\* The mutant is "none" or one of the two planted bugs.
ASSUME Mutant \in {"none", "CommitPassesSlot", "IngestPublishesEarly"}

\* Everyone who draws a sequence.
Writers == Commits \cup {Ingest}

\* Every key anyone writes.
Keys == {CommitKey} \cup IngestKeys

\* The keys writer w writes, every one at the sequence it drew.
KeysOf(w) == IF w = Ingest THEN IngestKeys ELSE {CommitKey}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The last sequence number handed out (the pipeline's state, After::next_seq,
  \* src/engine/commit/descriptor.rs).
  next,
  \* The "pending slots": sequence numbers that were handed out but whose data is not installed yet.
  \* Model bookkeeping: in the engine a slot is pending while the unit that took it runs.
  pending,
  \* The versions readers can find, as pairs <<key, sequence>>: what the memtables and tables hold.
  installed,
  \* The newest sequence a new snapshot may read at: the read horizon (RegolithEngine::visible_seq,
  \* a ReadHorizon in src/engine/read_horizon.rs).
  visible,
  \* Where each writer is: "idle", "drawn" (it has a sequence), "installed" (its data can be found)
  \* or "published" (the horizon reached its sequence).
  phase,
  \* The sequence each writer drew, 0 before it draws.
  wseq,
  \* The readers that have taken their snapshot.
  taken,
  \* The sequence each reader's snapshot reads at.
  snap,
  \* What each snapshot read for each key at the moment it was taken. Model bookkeeping: later reads
  \* through the same snapshot are compared with it.
  seen

\* Every variable, so a step that changes none of them is a stutter.
vars == <<next, pending, installed, visible, phase, wseq, taken, snap, seen>>

\* The greatest element of a finite, nonempty set of naturals.
Max(S) == CHOOSE m \in S : \A n \in S : n <= m

\* A read of key k at snapshot s: the newest installed version at or below
\* s, or 0 for absent (sequences start at 1).
ReadAt(s, k) ==
  \* vs: the sequences of the installed versions of k at or below s.
  LET vs == {v[2] : v \in {x \in installed : x[1] = k /\ x[2] <= s}}
  \* None: the key reads as absent. Otherwise the newest of them.
  IN IF vs = {} THEN 0 ELSE Max(vs)

\* The start: nothing drawn, installed or published, and no snapshot taken.
Init ==
  \* The counter has handed out nothing.
  /\ next      = 0
  \* No slot is pending.
  /\ pending   = {}
  \* Nothing is installed.
  /\ installed = {}
  \* The horizon is 0.
  /\ visible   = 0
  \* No writer has begun.
  /\ phase     = [w \in Writers |-> "idle"]
  \* No writer has a sequence.
  /\ wseq      = [w \in Writers |-> 0]
  \* No reader has taken a snapshot.
  /\ taken     = {}
  \* No snapshot sequence is set.
  /\ snap      = [r \in Readers |-> 0]
  \* No snapshot has read anything.
  /\ seen      = [r \in Readers |-> [k \in Keys |-> 0]]

\* Writer w draws the next sequence. It is a pending slot until w installs
\* its data. Mutant IngestPublishesEarly: the ingest also publishes it now.
Draw(w) ==
  \* The writer has not begun.
  /\ phase[w] = "idle"
  \* The counter hands out the next sequence.
  /\ next'    = next + 1
  \* The writer has it.
  /\ wseq'    = [wseq EXCEPT ![w] = next + 1]
  \* It is a pending slot from now on.
  /\ pending' = pending \cup {next + 1}
  \* The writer has drawn.
  /\ phase'   = [phase EXCEPT ![w] = "drawn"]
  \* Under the planted bug IngestPublishesEarly, the ingest raises the horizon to its sequence right
  \* now;
  /\ visible' = IF w = Ingest /\ Mutant = "IngestPublishesEarly"
                  \* otherwise the horizon stays.
                  THEN Max({visible, next + 1}) ELSE visible
  \* Nothing else changes.
  /\ UNCHANGED <<installed, taken, snap, seen>>

\* Writer w installs its versions: a commit's memtable insert, the ingest's
\* table added to the version. Its slot stops being pending.
Install(w) ==
  \* The writer has drawn a sequence.
  /\ phase[w]   = "drawn"
  \* Its versions, one per key it writes, all at its sequence, can now be found.
  /\ installed' = installed \cup {<<k, wseq[w]>> : k \in KeysOf(w)}
  \* Its slot is no longer pending.
  /\ pending'   = pending \ {wseq[w]}
  \* The writer has installed.
  /\ phase'     = [phase EXCEPT ![w] = "installed"]
  \* Nothing else changes.
  /\ UNCHANGED <<next, visible, wseq, taken, snap, seen>>

\* The fix's guard: no pending slot is at or below sequence s.
NoPendingAtOrBelow(s) == \A p \in pending : s < p

\* ReadHorizon::publish: writer w raises the horizon to its sequence, never
\* lowering it. The fix publishes only past no pending slot. Mutant
\* CommitPassesSlot: a commit publishes regardless.
Publish(w) ==
  \* The writer has installed its data.
  /\ phase[w] = "installed"
  \* No pending slot is at or below its sequence (the engine's rule),
  /\ \/ NoPendingAtOrBelow(wseq[w])
     \* or the planted bug CommitPassesSlot lets a commit publish anyway.
     \/ Mutant = "CommitPassesSlot" /\ w \in Commits
  \* The horizon rises to the writer's sequence, and never falls.
  /\ visible' = Max({visible, wseq[w]})
  \* The writer has published.
  /\ phase'   = [phase EXCEPT ![w] = "published"]
  \* Nothing else changes.
  /\ UNCHANGED <<next, pending, installed, wseq, taken, snap, seen>>

\* Reader r takes a snapshot at the horizon and records what it reads for
\* every key at that moment.
Snapshot(r) ==
  \* The reader has not taken its snapshot yet.
  /\ r \notin taken
  \* Now it has.
  /\ taken' = taken \cup {r}
  \* Its snapshot reads at the horizon.
  /\ snap'  = [snap EXCEPT ![r] = visible]
  \* It writes down what it reads for every key at this moment.
  /\ seen'  = [seen EXCEPT ![r] = [k \in Keys |-> ReadAt(visible, k)]]
  \* Nothing else changes.
  /\ UNCHANGED <<next, pending, installed, visible, phase, wseq>>

\* Every step the system can take.
Next ==
  \* A writer draws a sequence, installs its data, or publishes.
  \/ \E w \in Writers : Draw(w) \/ Install(w) \/ Publish(w)
  \* A reader takes its snapshot.
  \/ \E r \in Readers : Snapshot(r)

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The counter is a number.
  /\ next \in Nat
  \* Pending slots are sequences that were handed out.
  /\ pending \subseteq 1..next
  \* Installed versions are known keys at sequences that were handed out.
  /\ installed \subseteq Keys \X (1..next)
  \* The horizon is at most the counter.
  /\ visible \in 0..next
  \* Every writer is in one of the four phases.
  /\ phase \in [Writers -> {"idle", "drawn", "installed", "published"}]
  \* Every drawn sequence was handed out.
  /\ wseq \in [Writers -> 0..next]
  \* Only readers take snapshots.
  /\ taken \subseteq Readers
  \* Every snapshot sequence was handed out.
  /\ snap \in [Readers -> 0..next]

\* THE HEADLINE. A snapshot reads every key the same at every step after it
\* was taken. Lean: repeatable_snapshot.
RepeatableSnapshot ==
  \* For every reader that took a snapshot and every key: reading the key at the snapshot's sequence
  \* now gives what it gave when the snapshot was taken. Rules out, for example: a snapshot at
  \* sequence 2 reads ingested key 2 as absent, the ingest's table (sequence 1) is installed a
  \* moment later, and the same snapshot now reads key 2 as present.
  \A r \in taken : \A k \in Keys : ReadAt(snap[r], k) = seen[r][k]

\* The fix's invariant: the horizon is below every pending slot.
\* Lean: Inv.pending_above.
NoPendingAtOrBelowVisible == NoPendingAtOrBelow(visible)

====
