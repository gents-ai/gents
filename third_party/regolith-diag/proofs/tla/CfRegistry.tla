---- MODULE CfRegistry ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A column family is a named keyspace inside one database: its keys carry
\* the family's id in front. Creating a family writes its name to the
\* database's own metadata; dropping it writes one range tombstone over every
\* key with its id (deleting them all at once) and removes the name. The
\* registry in memory (CfRegistry, src/column_family.rs) says which ids are
\* live, and every write to a family checks it.
\*
\* Every write takes a sequence number, in order. A tombstone at sequence 7
\* deletes every key of the family written at sequences below 7.
\*
\* Tiny example. Writer W checks "family 5 is live" and goes to commit. A
\* drop of family 5 commits first, at sequence 7. If W's write now takes
\* sequence 8, it lands ABOVE the tombstone: it is visible, in a family no
\* handle can name any more, and no later compaction removes it. W was told
\* "ok" for bytes nobody can ever read.
\*
\* THE COMMIT PIPELINE (src/engine/commit/pipeline.rs). There is one path. A
\* write, a transaction's commit, an ingest's install, a create and a drop
\* each take a slot of the commit ring, and the slots are decided strictly in
\* ring order, each by whichever thread reaches it. Deciding a slot is where
\* it takes its sequence. A create, a drop and an ingest's install are
\* ordered units (ordered.rs): the unit runs when its slot's turn comes, with
\* every slot before it decided and none after it.
\*
\* The fix (src/engine/commit/families.rs): a write is checked against the
\* registry when its slot is DECIDED, just before it takes its sequence
\* (cf_fence, in decide_work), and a drop retires the family inside its own
\* unit, right after its tombstone. So a write whose slot comes before the
\* drop's takes a sequence below the tombstone (and the tombstone deletes
\* it), and one whose slot comes after finds the family gone and is refused.
\*
\* Two wrong places for the check, and their stories:
\*   - checked when the slot was TAKEN, not again when it is decided: the
\*     drop takes slot 4, W takes slot 5 (family live, W passes); the drop is
\*     decided (tombstone 7) and retires the family; W is decided on the
\*     strength of its earlier pass and lands at 8, over the tombstone;
\*   - a transaction's slot skips the check: the same story with W a
\*     transaction.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - a write lands after its family's tombstone (NoWriteAfterDrop);
\*   - a write lands before its family was created (NoWriteBeforeBirth);
\*   - a reader sees a family go back: live after dead, or unborn after
\*     live (LifeInOrder). Ids are never reused, which is what keeps this.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   life      a family id's stage: CfRegistry::by_id holds the id while
\*             it is live; "unborn" before CfRegistry::publish, "dead"
\*             after CfRegistry::retire
\*   seq       the last sequence the pipeline handed out (After::next_seq)
\*   born,tomb the sequence of the create's meta write and of the drop's
\*             range tombstone (Ordered::commit_alone)
\*   landed    every committed write, as <<family, sequence>>
\*   wpc       each writer's progress
\*   ring      the slots taken and not yet decided, in ring order: each a
\*             writer's, a create's or a drop's (Ring::reserve, install)
\*   asked     the creates and drops that took a slot (one each a family)
\*   passed    only the FenceAtInstall bug: the writer was checked when it
\*             took its slot
\*   seenLife  each reader's observations of one family, in order
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Who decides a slot. Any thread may, and two may compute one slot's
\*     decision at once; the slot keeps one (one compare-and-swap), and both
\*     were computed with every earlier slot decided and no later one, which
\*     is all the check reads. So deciding a slot is one step here.
\*     CommitPipeline.tla has the threads.
\*   - The name map: a name finds an id, but liveness is the id alone.
\*   - The appends a transaction numbers: they write the default family,
\*     which is never dropped.
\*   - The write, the sync and the publication of a decided slot: none of
\*     them changes a sequence. CommitPipeline.tla has them.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_CfRegistry_Green              a plain writer, a transaction and an
\*                                    ingest on one family that is created
\*                                    and dropped, in every ring order, a
\*                                    reader watching.
\*   MC_CfRegistry_Red_FenceOutside   the family is checked only at the API
\*                                    boundary, not when the slot is
\*                                    decided: NoWriteAfterDrop breaks.
\*   MC_CfRegistry_Red_RetireLate     the drop retires the family after its
\*                                    unit, so writes decided in between land
\*                                    past the tombstone: NoWriteAfterDrop
\*                                    breaks.
\*   MC_CfRegistry_Red_ReuseId        a create may take a dropped family's
\*                                    id again: LifeInOrder breaks.
\*   MC_CfRegistry_Red_FenceAtInstall a writer is checked when it takes its
\*                                    slot, not when the slot is decided:
\*                                    NoWriteAfterDrop breaks.
\*   MC_CfRegistry_Red_TxnUnfenced    a transaction's slot is not checked:
\*                                    NoWriteAfterDrop breaks.
\*
\* Lean, proofs/lean/Regolith/CfRegistry.lean, proves the same rules for
\* every number of families, writers, slots and steps.

\* We use numbers, sequences and finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The family ids.
  Families,
  \* The writers, numbered from 1.
  Writers,
  \* [Writers -> Families]: the family each writer writes to.
  WriteTo,
  \* [Writers -> {"plain", "txn", "ingest"}]: how each writer writes: a
  \* plain write or batch, an optimistic transaction's commit, or an ingest.
  Kind,
  \* The readers that watch a family's life.
  Readers,
  \* [Readers -> Families]: the family each reader watches.
  Watch,
  \* How many observations a reader makes.
  Looks,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* Every writer writes to one family.
ASSUME WriteTo \in [Writers -> Families]
\* Every writer writes one way.
ASSUME Kind \in [Writers -> {"plain", "txn", "ingest"}]
\* Writers are numbered from 1.
ASSUME Writers \subseteq Nat \ {0}
\* Every reader watches one family.
ASSUME Watch \in [Readers -> Families]
\* The bug names this model knows.
ASSUME Mutant \in {"none", "FenceOutside", "RetireLate", "ReuseId",
                   "FenceAtInstall", "TxnUnfenced"}

\* For the configurations: every writer and reader is on family 1.
OnFamilyOne(S) == [x \in S |-> 1]
\* Writers 1, 2 and 3 all write to family 1.
WriteToThree == OnFamilyOne({1, 2, 3})
\* Writers 1 and 2 both write to family 1.
WriteToOne == OnFamilyOne({1, 2})
\* Writer 1 alone writes to family 1.
WriteToSolo == OnFamilyOne({1})
\* Reader 1 watches family 1.
WatchOne == OnFamilyOne({1})
\* No writer at all.
NoWrites == OnFamilyOne({})
\* Writer 1 writes plainly, writer 2 commits a transaction, writer 3 ingests.
KindsMixed == [w \in {1, 2, 3} |-> IF w = 1 THEN "plain" ELSE IF w = 2 THEN "txn" ELSE "ingest"]
\* Writers 1 and 2 both write plainly.
KindsPlain == [w \in {1, 2} |-> "plain"]
\* Writer 1 commits a transaction.
KindsTxn == [w \in {1} |-> "txn"]
\* No writer, so no kinds.
NoKinds == [w \in {} |-> "plain"]

\* A family's stages, in the only order the code moves them.
Rank(stage) ==
  \* Unborn comes first ...
  CASE stage = "unborn" -> 0
  \* ... then live ...
  []   stage = "live"   -> 1
  \* ... then dead, for good.
  []   stage = "dead"   -> 2

\* What a slot of the ring carries: a writer's write, or a create or a drop
\* of a family.
SlotOf(kind, x) == [kind |-> kind, of |-> x]
\* Every slot there can be.
AllSlots ==
  \* A writer's ...
  {SlotOf("write", w) : w \in Writers}
    \* ... or a create's or a drop's.
    \cup {SlotOf(k, f) : k \in {"create", "drop"}, f \in Families}

\* The state that changes from step to step.
VARIABLES
  \* [Families -> {"unborn","live","dead"}]: what the registry says.
  life,
  \* The last sequence number handed out.
  seq,
  \* [Families -> Nat]: the sequence of the create (0: not created).
  born,
  \* [Families -> Nat]: the sequence of the drop's tombstone (0: none).
  tomb,
  \* [Families -> BOOLEAN]: bug RetireLate only: the tombstone committed
  \* and the retire has not happened yet.
  retiring,
  \* The committed writes, as <<family, sequence>>.
  landed,
  \* [Writers -> {"start","checked","queued","done","refused"}]: each
  \* writer's step. "queued" is a writer whose slot is in the ring.
  wpc,
  \* The slots taken and not yet decided, oldest first.
  ring,
  \* The creates and drops that took a slot, as slots.
  asked,
  \* Bug FenceAtInstall only: the writers checked when they took their slot.
  passed,
  \* [Readers -> Seq(stage)]: what each reader saw, in order.
  seenLife

\* All the variables, for "nothing else changes".
vars == <<life, seq, born, tomb, retiring, landed, wpc, ring, asked, passed, seenLife>>

\* Every variable holds the kind of value it should.
TypeOK ==
  \* Each family is in one stage.
  /\ life \in [Families -> {"unborn", "live", "dead"}]
  \* The sequence counter is a number.
  /\ seq \in Nat
  \* Births and tombstones are sequence numbers or 0.
  /\ born \in [Families -> Nat]
  \* Each family's tombstone is a sequence number, or 0 for none.
  /\ tomb \in [Families -> Nat]
  \* The late-retire flag is a yes or a no per family.
  /\ retiring \in [Families -> BOOLEAN]
  \* Landed writes are (family, sequence) pairs.
  /\ landed \subseteq (Families \X Nat)
  \* Each writer is at one of its steps.
  /\ wpc \in [Writers -> {"start", "checked", "queued", "done", "refused"}]
  \* The ring holds slots.
  /\ ring \in Seq(AllSlots)
  \* The creates and drops asked for are slots, and the bug's marks are writers.
  /\ asked \subseteq AllSlots /\ passed \subseteq Writers

\* The start: no family created, nothing written, no slot taken, nothing seen.
Init ==
  \* Every family id is unborn.
  /\ life = [f \in Families |-> "unborn"]
  \* No sequence handed out yet.
  /\ seq = 0
  \* No create, no tombstone.
  /\ born = [f \in Families |-> 0]
  \* No family has a tombstone.
  /\ tomb = [f \in Families |-> 0]
  \* Nothing is waiting to be retired.
  /\ retiring = [f \in Families |-> FALSE]
  \* Nothing has been written.
  /\ landed = {}
  \* Every writer is about to write.
  /\ wpc = [w \in Writers |-> "start"]
  \* No slot is taken.
  /\ ring = <<>>
  \* No create or drop was asked for, and no writer carries the bug's mark.
  /\ asked = {} /\ passed = {}
  \* No reader has looked yet.
  /\ seenLife = [r \in Readers |-> <<>>]

\* The slots in the ring, as a set.
Waiting == {ring[i] : i \in 1..Len(ring)}

\* A create or a drop of family f takes a slot of the ring
\* (RegolithEngine::create_family, drop_family: each runs its body as an
\* ordered unit). What it does is decided when its turn comes. The real code
\* asks for each once a family; the bug ReuseId may ask for a second create.
Ask(kind, f) ==
  \* Not asked for yet, or (bug) a create of an id that is dead.
  /\ \/ SlotOf(kind, f) \notin asked
     \* Under the bug ReuseId, a create of a dropped id may be asked again.
     \/ Mutant = "ReuseId" /\ kind = "create" /\ life[f] = "dead" /\ SlotOf(kind, f) \notin Waiting
  \* Its slot joins the end of the ring.
  /\ ring' = Append(ring, SlotOf(kind, f))
  \* It is asked for.
  /\ asked' = asked \cup {SlotOf(kind, f)}
  \* Nothing else changes.
  /\ UNCHANGED <<life, seq, born, tomb, retiring, landed, wpc, passed, seenLife>>

\* Writer w's early check at the API boundary (Db::put_cf's
\* validate_cf_handle; an ingest's key validation): a family that is not
\* live is refused at once.
ApiCheck(w) ==
  \* The writer is about to write.
  /\ wpc[w] = "start"
  \* It passes if the family is live now, and is refused otherwise.
  /\ wpc' = [wpc EXCEPT ![w] = IF life[WriteTo[w]] = "live" THEN "checked" ELSE "refused"]
  \* Nothing else changes.
  /\ UNCHANGED <<life, seq, born, tomb, retiring, landed, ring, asked, passed, seenLife>>

\* Writer w takes a slot of the ring (enter: Ring::reserve, Ring::install).
\* The real code checks nothing here. The bug FenceAtInstall checks the
\* family now, refusing the writer if it is gone and marking it passed
\* otherwise.
Enter(w) ==
  \* The writer passed the API check.
  /\ wpc[w] = "checked"
  \* Under the bug FenceAtInstall: is the family no longer live?
  /\ IF Mutant = "FenceAtInstall" /\ life[WriteTo[w]] # "live"
     \* The bug's check refuses it, and it takes no slot.
     THEN /\ wpc' = [wpc EXCEPT ![w] = "refused"]
          \* The ring and the marks are as they were.
          /\ UNCHANGED <<ring, passed>>
     \* Otherwise its slot joins the end of the ring.
     ELSE /\ ring' = Append(ring, SlotOf("write", w))
          \* The writer waits for its slot's turn.
          /\ wpc' = [wpc EXCEPT ![w] = "queued"]
          \* Under the bug it is marked as having passed its check.
          /\ passed' = IF Mutant = "FenceAtInstall" THEN passed \cup {w} ELSE passed
  \* Nothing else changes.
  /\ UNCHANGED <<life, seq, born, tomb, retiring, landed, asked, seenLife>>

\* Whether writer m passes the check when its slot is decided. The real code
\* checks the registry right then (cf_fence in decide_work; cf_fence_ids in
\* an ingest's unit). The bugs: FenceOutside checks nobody; TxnUnfenced skips
\* transactions; FenceAtInstall takes the check made when the slot was taken
\* as its pass.
Passes(m) ==
  \* Bug FenceOutside: every writer passes, whatever the family is now.
  CASE Mutant = "FenceOutside" -> TRUE
  \* Bug TxnUnfenced: a transaction passes unchecked.
  []   Mutant = "TxnUnfenced" /\ Kind[m] = "txn" -> TRUE
  \* Bug FenceAtInstall: the writer passed its check already.
  []   Mutant = "FenceAtInstall" /\ m \in passed -> TRUE
  \* The real code: the family is live right now, at the slot's turn.
  []   OTHER -> life[WriteTo[m]] = "live"

\* A writer's slot is decided: the check, then the next sequence, or a
\* refusal that takes none (RegolithEngine::decide_work).
DecideWrite(w) ==
  \* The check, at the slot's turn.
  IF Passes(w)
  \* It takes the next sequence and lands in its family.
  THEN /\ seq' = seq + 1
       \* The write is in its family at that sequence.
       /\ landed' = landed \cup {<<WriteTo[w], seq + 1>>}
       \* The writer learns it landed.
       /\ wpc' = [wpc EXCEPT ![w] = "done"]
       \* The registry is untouched.
       /\ UNCHANGED <<life, born, tomb, retiring>>
  \* Refused: no sequence, nothing lands.
  ELSE /\ wpc' = [wpc EXCEPT ![w] = "refused"]
       \* Nothing else moves.
       /\ UNCHANGED <<life, seq, born, tomb, retiring, landed>>

\* A create's unit runs at its turn: its meta write takes the next sequence
\* and the registry publishes the id. An id that is not new is left alone
\* (the create returns the family that is there, or is refused). The bug
\* ReuseId also creates an id that was dropped.
DecideCreate(f) ==
  \* The id is new, or (bug) dead and taken again.
  IF life[f] = "unborn" \/ (Mutant = "ReuseId" /\ life[f] = "dead")
  \* The meta write takes the next sequence.
  THEN /\ seq' = seq + 1
       \* That is the family's birth.
       /\ born' = [born EXCEPT ![f] = seq + 1]
       \* The registry now holds the id: the family is live.
       /\ life' = [life EXCEPT ![f] = "live"]
       \* A reused id starts with no tombstone.
       /\ tomb' = [tomb EXCEPT ![f] = 0]
       \* Nothing else moves.
       /\ UNCHANGED <<retiring, landed, wpc>>
  \* Not new: nothing changes.
  ELSE UNCHANGED <<life, seq, born, tomb, retiring, landed, wpc>>

\* A drop's unit runs at its turn: its tombstone takes the next sequence and,
\* in the same unit, the registry retires the id. A family that is not live
\* is not dropped (a stale handle is refused). The bug RetireLate leaves the
\* retire for a later step of its own.
DecideDrop(f) ==
  \* Only a live family is dropped, and not twice.
  IF life[f] = "live" /\ ~retiring[f]
  \* The tombstone takes the next sequence.
  THEN /\ seq' = seq + 1
       \* The family's tombstone is that sequence.
       /\ tomb' = [tomb EXCEPT ![f] = seq + 1]
       \* The real code retires at once; the bug only marks it as pending.
       /\ IF Mutant = "RetireLate"
          \* The bug: the id is only marked as waiting to be retired,
          THEN /\ retiring' = [retiring EXCEPT ![f] = TRUE]
               \* and stays live.
               /\ UNCHANGED life
          \* The design: the id is dead at once.
          ELSE /\ life' = [life EXCEPT ![f] = "dead"]
               \* Nothing waits to be retired.
               /\ UNCHANGED retiring
       \* Nothing else moves.
       /\ UNCHANGED <<born, landed, wpc>>
  \* Not live: nothing changes.
  ELSE UNCHANGED <<life, seq, born, tomb, retiring, landed, wpc>>

\* The oldest undecided slot is decided: slots are decided strictly in ring
\* order (CommitPipeline.tla, DecidedIsPrefix), so this is the only one that
\* can be.
Decide ==
  \* There is an undecided slot.
  /\ ring # <<>>
  \* What it carries says what deciding it does.
  /\ LET slot == Head(ring) IN
       \* A write, a create or a drop.
       CASE slot.kind = "write" -> DecideWrite(slot.of)
       \* A create's unit.
       []   slot.kind = "create" -> DecideCreate(slot.of)
       \* A drop's unit.
       []   slot.kind = "drop" -> DecideDrop(slot.of)
  \* It leaves the ring.
  /\ ring' = Tail(ring)
  \* Nothing else changes.
  /\ UNCHANGED <<asked, passed, seenLife>>

\* Bug RetireLate: the retire, as a separate step after the drop's unit.
RetireLater(f) ==
  \* The tombstone committed and the retire is pending.
  /\ retiring[f]
  \* Now the registry drops the id.
  /\ life' = [life EXCEPT ![f] = "dead"]
  \* It no longer waits to be retired.
  /\ retiring' = [retiring EXCEPT ![f] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, born, tomb, landed, wpc, ring, asked, passed, seenLife>>

\* Reader r looks at its family in the registry and notes what it saw.
Look(r) ==
  \* It has looks left.
  /\ Len(seenLife[r]) < Looks
  \* It notes the family's stage now.
  /\ seenLife' = [seenLife EXCEPT ![r] = Append(seenLife[r], life[Watch[r]])]
  \* Nothing else changes.
  /\ UNCHANGED <<life, seq, born, tomb, retiring, landed, wpc, ring, asked, passed>>

\* Every step anyone can take.
Next ==
  \* A create or a drop takes a slot, or the bug's late retire happens.
  \/ \E f \in Families : Ask("create", f) \/ Ask("drop", f) \/ RetireLater(f)
  \* A writer checks at the boundary, or takes a slot.
  \/ \E w \in Writers : ApiCheck(w) \/ Enter(w)
  \* The oldest undecided slot is decided.
  \/ Decide
  \* A reader looks.
  \/ \E r \in Readers : Look(r)

\* A bound on the run for TLC: the real code creates and drops an id once,
\* but the bug ReuseId could go on forever.
Bounded == seq <= 8

\* The behaviours: start in Init, take Next steps.
Spec == Init /\ [][Next]_vars

\* ===========================================================================
\* THE RULES
\* ===========================================================================

\* Every write that landed in a dropped family is below its tombstone, so
\* the tombstone deletes it. Rules out: W checked "live", the drop
\* committed at 7, W landed at 8, visible in a family nobody can read.
NoWriteAfterDrop ==
  \* Each landed write (family, sequence): its family has no tombstone, or the
  \* write's sequence is below the tombstone's.
  \A pair \in landed : tomb[pair[1]] = 0 \/ pair[2] < tomb[pair[1]]

\* Every write that landed is above its family's birth. Rules out: a write
\* to an id that was not created yet.
NoWriteBeforeBirth ==
  \* Each landed write: its family was created, at a sequence below the write.
  \A pair \in landed : born[pair[1]] # 0 /\ born[pair[1]] < pair[2]

\* What a reader saw never goes back: unborn, then live, then dead. Rules
\* out: a reader that saw a family dropped and then, through the same id,
\* saw it live again, with a new family's data.
LifeInOrder ==
  \* For every reader ...
  \A r \in Readers :
    \* ... and every look it made ...
    \A i \in 1..Len(seenLife[r]) :
      \* ... and every later look ...
      \A j \in i..Len(seenLife[r]) :
        \* ... the later look saw the same stage or a later one.
        Rank(seenLife[r][i]) <= Rank(seenLife[r][j])

====
