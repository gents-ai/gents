---- MODULE ReadViewPublishers ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* ReadView.tla checks the read view's cell: a reader never holds a freed
\* view, and one compare-and-swap (CAS) publication never overwrites
\* another. It treats a view as a number. This model looks inside the view,
\* at the publishers the commit pipeline and the flush off the commit path
\* have, and checks that each of them publishing through the CAS
\* (ReadViewCell::publish, src/engine/read_view.rs) loses no data.
\*
\* A view is three things: the active memtable writers append to, the frozen
\* memtables waiting for a flush (oldest first), and the tables of the
\* current version. Data lives in exactly those three places.
\*
\* The publishers, and what each one already holds, so which of them can
\* race:
\*   - a rotation seals the active memtable behind a fresh one
\*     (Ordered::seal, src/engine/commit/ordered.rs). It runs in an ordered
\*     unit of the commit pipeline: the thread that runs the unit's body
\*     holds the round, every earlier commit is published and no later one
\*     is decided until the body returns;
\*   - a rollback, after the log failed (roll_back, same file), replaces the
\*     active memtable by a copy without the commits it discards. The thread
\*     that rolls back holds the round too, so a rotation and a rollback
\*     never run at once;
\*   - a flush, by the compaction worker, by a writer after its commit, or
\*     by a rotation that has to make room first (Flusher::flush_oldest),
\*     takes the oldest frozen memtable, writes its table, and installs the
\*     table and retires the memtable in ONE version publish
\*     (VersionGuard::retire_with_version). One flush runs at a time (the
\*     flush exclusion, Flusher::flushing), and it does not need the round;
\*   - a compaction installs its output, and an ingest installs its table.
\* Every version publish is the last thing a section of the version store's
\* step does (VersionStore::run, VersionGuard::drop). The step runs one
\* section at a time, on whichever thread holds it, so two version publishes
\* never race. There is no mutex here: a thread that finds the step held
\* leaves its section for the holder to run.
\* drop_all also publishes, but it runs in an ordered unit that holds the
\* flush exclusion and the compaction gate, so it races none of these and is
\* left out.
\*
\* So a rotation or a rollback can race a flush's retire, and either can
\* race a version publish. Each publisher builds its next view from the view
\* it loaded and swaps it in only if that view is still the published one;
\* otherwise it builds again on what won.
\*
\* Tiny example of what goes wrong without the CAS. An ingest installs its
\* table while a rollback is at work. The view is (active 1, frozen [],
\* tables {}). The rollback loads it and builds (active 2, frozen [],
\* tables {}): memtable 2 is the copy of 1 without the discarded commits.
\* Meanwhile the ingest publishes (active 1, frozen [], tables {101}) and
\* tells its caller the table is in. If the rollback now stores its view
\* without the CAS, the published view is (active 2, frozen [], tables {}):
\* the ingested table is in no view, and every read misses its rows.
\*
\* A flush that published its table first and retired its memtable in a
\* second publication left a view between the two that held both: every
\* entry twice, at the same sequence. A read collects a key's merge operands
\* from every source, so it applied each operand twice ("ab" read as "abab").
\* That is why the two are one publication.
\*
\* What this model checks cannot happen:
\*   - a memtable's data is in no published place (NoLostMemtable);
\*   - a memtable's data is in two published places at once, the memtable
\*     and its table (NoDoubleSource);
\*   - an ingested table is not in the published version (NoLostIngest);
\*   - a memtable a flush retired comes back (RetiredStayGone), which would
\*     flush it twice and install an old table over a newer one;
\*   - a memtable a rollback replaced comes back (ReplacedStayGone), which
\*     would bring back the commits the rollback discarded;
\*   - the frozen list leaves seal order (FrozenInSealOrder), which a flush
\*     relies on: it installs the oldest first (LsmOrder.tla).
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   pub        ReadViewCell::current: [active, frozen, tables]
\*   nextMt     the id the next fresh memtable gets (MemTable::new, or the
\*              rollback's rebuild_memtable, before the publication)
\*   ingested   how many tables ingests installed; ingest n is table 100+n
\*   round      who holds the pipeline's round: nobody, a rotation's unit,
\*              or a rollback
\*   sPhase...  the rotation's progress inside ReadViewCell::publish: idle,
\*              or built (it loaded sBase and built sNext)
\*   rPhase...  the rollback's progress: idle, or built; rOld is the
\*              memtable it replaces
\*   fPhase...  the flush's progress: idle, picked (it took the oldest
\*              frozen memtable fTarget), installBuilt, and, only with the
\*              split publication of the RED cases, installed and
\*              retireBuilt
\*   cPhase...  the compaction or ingest's progress: idle, built, or
\*              retry (its swap lost and it builds again)
\*   step       whose section the version store's step is running: nobody's,
\*              the flush's install, or a compaction's or an ingest's
\*   retired    a ghost: every memtable a flush retired
\*   replaced   a ghost: every memtable a rollback replaced by its copy
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Readers and reclamation: ReadView.tla checks those for any content.
\*   - Keys: a memtable or a table stands for all the data it holds. A
\*     rollback's copy stands for the memtable it replaces, less the
\*     discarded commits, which no reader was ever shown
\*     (CommitPipeline.tla, the published prefix).
\*   - A compaction's merge: it replaces tables by one holding the same
\*     data, so the set of places data lives in does not change.
\*   - How a section reaches the step, and who runs it: VersionStep.tla.
\*     Here a section is in the step from its first build to its swap.
\*   - A unit that holds the step or the flush exclusion for its whole body
\*     (a poll's, src/engine/commit/park.rs): that only takes interleavings
\*     away from the ones checked here.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_ReadViewPublishers_Green              every publisher swaps by CAS.
\*   MC_ReadViewPublishers_Red_SplitRetire    a flush publishes its table,
\*                                            then retires its memtable in a
\*                                            second publication: the view
\*                                            between holds both,
\*                                            NoDoubleSource breaks.
\*   MC_ReadViewPublishers_Red_PlainRetire    the split publication, with
\*                                            the retire stored without the
\*                                            CAS: a seal racing it is lost,
\*                                            NoLostMemtable breaks.
\*   MC_ReadViewPublishers_Red_PlainVersion   a compaction's or an ingest's
\*                                            version publish stores without
\*                                            the CAS: a seal racing it is
\*                                            lost, NoLostMemtable breaks.
\*   MC_ReadViewPublishers_Red_PlainRollback  a rollback stores its view
\*                                            without the CAS: an ingest
\*                                            racing it is lost,
\*                                            NoLostIngest breaks.
\*   MC_ReadViewPublishers_Red_PlainRollbackFlush
\*                                            the same bug against a flush:
\*                                            the memtable it retired is
\*                                            frozen again, RetiredStayGone
\*                                            breaks.
\*   MC_ReadViewPublishers_Red_SealOutsideRound
\*                                            a rotation that does not hold
\*                                            the round seals the memtable a
\*                                            rollback is replacing: it is
\*                                            frozen, and its discarded
\*                                            commits get flushed,
\*                                            ReplacedStayGone breaks.
\*
\* Lean, proofs/lean/Regolith/ReadViewPublishers.lean, proves the same
\* rules for every number of publications.

\* Numbers, sequences and finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The most memtables the model creates (the first one included).
  MaxMemtables,
  \* The most tables ingests install.
  MaxIngests,
  \* The most compactions that install.
  MaxCompactions,
  \* The most rollbacks.
  MaxRollbacks,
  \* "Green" for the real code, or the name of one planted bug.
  Mode

\* The bug names this model knows.
ASSUME Mode \in {"Green", "SplitRetire", "PlainRetire", "PlainVersion", "PlainRollback",
                 "SealOutsideRound"}

\* The bugs whose flush publishes its table and retires its memtable apart.
Split == Mode \in {"SplitRetire", "PlainRetire"}

\* The table id ingest n installs: well apart from memtable ids.
IngestTable(n) == 100 + n

\* The memtables in a frozen list, as a set.
Range(s) == {s[i] : i \in 1..Len(s)}

\* `s` without the element `x`, order kept (retire_memtable, by identity).
Without(s, x) == SelectSeq(s, LAMBDA y : y # x)

\* The state that changes from step to step.
VARIABLES
  \* The published view.
  pub,
  \* The id the next fresh memtable gets.
  nextMt,
  \* How many tables ingests have installed.
  ingested,
  \* How many compactions have installed.
  compacted,
  \* How many rollbacks have published.
  rolled,
  \* Who holds the pipeline's round: "none", "seal" or "rollback".
  round,
  \* The rotation: "idle" or "built", the view it built from, and the one
  \* it built.
  sPhase, sBase, sNext,
  \* The rollback: "idle" or "built", the memtable it replaces, the view it
  \* built from, and the one it built.
  rPhase, rOld, rBase, rNext,
  \* The flush: its step, the memtable it flushes, the view it built from,
  \* and the one it built.
  fPhase, fTarget, fBase, fNext,
  \* The compaction or ingest: "idle", "built" or "retry"; whether it is
  \* an ingest; the view it built from, and the one it built.
  cPhase, cIngest, cBase, cNext,
  \* Whose section the version store's step runs: "none", "flush" or
  \* "version".
  step,
  \* Ghost: every memtable a flush retired.
  retired,
  \* Ghost: every memtable a rollback replaced by its copy.
  replaced

\* The rotation's variables.
sealVars == <<sPhase, sBase, sNext>>

\* The rollback's variables.
rollVars == <<rPhase, rOld, rBase, rNext>>

\* The flush's variables.
flushVars == <<fPhase, fTarget, fBase, fNext>>

\* The compaction's or ingest's variables.
verVars == <<cPhase, cIngest, cBase, cNext>>

\* Every variable.
vars == <<pub, nextMt, ingested, compacted, rolled, round, sealVars, rollVars, flushVars,
          verVars, step, retired, replaced>>

\* A view with nothing in it, the starting value of the publishers' scratch.
NoView == [active |-> 0, frozen |-> <<>>, tables |-> {}]

\* The start: memtable 1 is active, nothing is frozen or in a table.
Init ==
  \* Memtable 1 takes the writes.
  /\ pub = [active |-> 1, frozen |-> <<>>, tables |-> {}]
  \* The next fresh memtable is 2.
  /\ nextMt = 2
  \* Nothing ingested yet.
  /\ ingested = 0
  \* No compaction has run.
  /\ compacted = 0
  \* No rollback has run.
  /\ rolled = 0
  \* Nobody holds the round.
  /\ round = "none"
  \* The rotation is idle, with nothing built.
  /\ sPhase = "idle" /\ sBase = NoView /\ sNext = NoView
  \* The rollback is idle, with no memtable to replace and nothing built.
  /\ rPhase = "idle" /\ rOld = 0 /\ rBase = NoView /\ rNext = NoView
  \* The flush is idle, with no target and nothing built.
  /\ fPhase = "idle" /\ fTarget = 0 /\ fBase = NoView /\ fNext = NoView
  \* The version publisher (a compaction or an ingest) is idle, with nothing built.
  /\ cPhase = "idle" /\ cIngest = FALSE /\ cBase = NoView /\ cNext = NoView
  \* The version store's step runs nobody's section.
  /\ step = "none"
  \* Nothing has been retired.
  /\ retired = {}
  \* Nothing has been replaced.
  /\ replaced = {}

\* Whether a publisher's swap goes in. The real code: only if the view it
\* built from is still the published one. A plain store always goes in.
Swaps(base, plain) == plain \/ pub = base

\* ---- The rotation (in an ordered unit: it holds the round) ----------------

\* Whether the rotation may run: the round is free or its own. The bug
\* SealOutsideRound lets it run whoever holds the round.
SealMayRun == Mode = "SealOutsideRound" \/ round \in {"none", "seal"}

\* The round after the rotation took it. The bug SealOutsideRound never
\* takes it.
SealRound == IF Mode = "SealOutsideRound" THEN round ELSE "seal"

\* The rotation loads the published view and builds the next: the active
\* memtable joins the end of the frozen list and the fresh one, made before
\* the publication, becomes active (Ordered::seal's closure).
SealBuild ==
  \* Idle, and a fresh memtable may still be made.
  /\ sPhase = "idle"
  \* (The budget of memtables is not spent.)
  /\ nextMt <= MaxMemtables
  \* The unit's body runs with the round held.
  /\ SealMayRun
  \* It holds the round until its swap goes in.
  /\ round' = SealRound
  \* It built from the view published now.
  /\ sBase' = pub
  \* The fresh memtable is active; the sealed one is the newest frozen.
  /\ sNext' = [active |-> nextMt, frozen |-> Append(pub.frozen, pub.active),
               tables |-> pub.tables]
  \* The rotation has built its view.
  /\ sPhase' = "built"
  \* Nothing else changes.
  /\ UNCHANGED <<pub, nextMt, ingested, compacted, rolled, rollVars, flushVars, verVars,
                 step, retired, replaced>>

\* The rotation's compare-and-swap. In, the fresh memtable is used up and
\* the unit ends; out, it builds again on what won (the closure runs again),
\* the round still its own.
SealSwap ==
  \* It built a view.
  /\ sPhase = "built"
  \* The rotation always swaps by CAS: no bug here touches that.
  /\ IF Swaps(sBase, FALSE)
     \* In: its view is published, and the next fresh memtable is a new one.
     THEN /\ pub' = sNext
          \* The next fresh memtable takes the next number.
          /\ nextMt' = nextMt + 1
          \* The unit's body is over: the round is free, unless the bug
          \* never took it.
          /\ round' = IF Mode = "SealOutsideRound" THEN round ELSE "none"
     \* Out: nothing changes, and it builds again.
     ELSE UNCHANGED <<pub, nextMt, round>>
  \* Either way the attempt is over.
  /\ sPhase' = "idle"
  \* Nothing else changes.
  /\ UNCHANGED <<sBase, sNext, ingested, compacted, rolled, rollVars, flushVars, verVars,
                 step, retired, replaced>>

\* ---- The rollback (by the thread that holds the round) --------------------

\* The rollback loads the published view and builds the next: the copy,
\* made before the publication, takes the place of the active memtable it
\* was made from; the frozen list and the version are the loaded view's
\* (roll_back's closure). A view whose active memtable is not the one it
\* replaces is left as it is.
RollbackBuild ==
  \* Idle, with a rollback and a memtable left in the budget.
  /\ rPhase = "idle"
  \* (The budget of rollbacks is not spent.)
  /\ rolled < MaxRollbacks
  \* (The budget of memtables is not spent.)
  /\ nextMt <= MaxMemtables
  \* The round is free, or already its own (it builds again).
  /\ round \in {"none", "rollback"}
  \* It holds the round until its swap goes in.
  /\ round' = "rollback"
  \* The memtable it replaces: the pipeline's, read once when the rollback
  \* begins, which is the active one of the view published then.
  /\ rOld' = IF round = "none" THEN pub.active ELSE rOld
  \* It built from the view published now.
  /\ rBase' = pub
  \* The copy is active where the replaced memtable was.
  /\ rNext' = [pub EXCEPT !.active = IF pub.active = rOld' THEN nextMt ELSE pub.active]
  \* The rollback has built its view.
  /\ rPhase' = "built"
  \* Nothing else changes.
  /\ UNCHANGED <<pub, nextMt, ingested, compacted, rolled, sealVars, flushVars, verVars,
                 step, retired, replaced>>

\* The rollback's swap, which the PlainRollback bug stores. In, the copy is
\* used up, the replaced memtable is noted and the round is free; out, it
\* builds again on what won, the round still its own.
RollbackSwap ==
  \* It built a view.
  /\ rPhase = "built"
  \* Does the swap go in? The bug PlainRollback stores without the
  \* compare-and-swap, so it always does.
  /\ IF Swaps(rBase, Mode = "PlainRollback")
     \* In: its view is published.
     THEN /\ pub' = rNext
          \* The copy took the next number.
          /\ nextMt' = nextMt + 1
          \* The memtable it replaced is noted.
          /\ replaced' = replaced \cup {rOld}
          \* One more rollback is done.
          /\ rolled' = rolled + 1
          \* The round is free.
          /\ round' = "none"
     \* Out: nothing changes, and it builds again.
     ELSE UNCHANGED <<pub, nextMt, replaced, rolled, round>>
  \* Either way the attempt is over.
  /\ rPhase' = "idle"
  \* Nothing else changes.
  /\ UNCHANGED <<rOld, rBase, rNext, ingested, compacted, sealVars, flushVars, verVars,
                 step, retired>>

\* ---- The flush (worker, after a commit, or making room for a rotation) ----

\* The flush takes the oldest frozen memtable (flush_oldest_inner reads
\* `frozen.first()`) and writes its table, which publishes nothing.
FlushPick ==
  \* Idle, and something is frozen.
  /\ fPhase = "idle"
  \* (At least one memtable is frozen.)
  /\ pub.frozen # <<>>
  \* The oldest frozen memtable.
  /\ fTarget' = Head(pub.frozen)
  \* The flush has picked its target.
  /\ fPhase' = "picked"
  \* Nothing else changes.
  /\ UNCHANGED <<pub, nextMt, ingested, compacted, rolled, round, sealVars, rollVars,
                 fBase, fNext, verVars, step, retired, replaced>>

\* The flush installs its table: in a section of the version store's step,
\* it builds a view whose version holds the new table (VersionSet::apply of
\* AddFile, then VersionGuard::drop's publication), and whose frozen list no
\* longer holds the memtable (retire_with_version). The split bugs leave the
\* memtable frozen for a retire of its own.
FlushInstallBuild ==
  \* The table is written, and the step is free or already runs this section.
  /\ fPhase = "picked"
  \* (The step runs no other section.)
  /\ step \in {"none", "flush"}
  \* Its section runs until its swap goes in.
  /\ step' = "flush"
  \* It built from the view published now, adding its table.
  /\ fBase' = pub
  \* The view it built:
  /\ fNext' = IF Split
               \* The split bugs: the table only.
               THEN [pub EXCEPT !.tables = pub.tables \cup {fTarget}]
               \* The real code: the table, and the memtable gone, together.
               ELSE [pub EXCEPT !.tables = pub.tables \cup {fTarget},
                                !.frozen = Without(pub.frozen, fTarget)]
  \* The flush has built its install.
  /\ fPhase' = "installBuilt"
  \* Nothing else changes.
  /\ UNCHANGED <<pub, nextMt, ingested, compacted, rolled, round, sealVars, rollVars,
                 fTarget, verVars, retired, replaced>>

\* The install's swap: a version publish, so the PlainVersion bug stores it.
\* In: the table is published and the section ends. Out: it builds again,
\* its section still the one in the step.
FlushInstallSwap ==
  \* It built its view.
  /\ fPhase = "installBuilt"
  \* Does the swap go in? The bug PlainVersion stores without the compare-and-swap, so it always
  \* does.
  /\ IF Swaps(fBase, Mode = "PlainVersion")
     \* In: the version with the table is published; the section is over.
     THEN /\ pub' = fNext
          \* The step is free again.
          /\ step' = "none"
          \* The real code retired the memtable in it: the flush is done.
          \* The split bugs still owe the retire.
          /\ IF Split
             \* The split bugs: the table is in, and the retire is still owed.
             THEN /\ fPhase' = "installed"
                  \* Nothing is retired yet.
                  /\ UNCHANGED retired
             \* The design: the flush is done.
             ELSE /\ fPhase' = "idle"
                  \* Its memtable is recorded as retired.
                  /\ retired' = retired \cup {fTarget}
     \* Out: back to build, its section still in the step.
     ELSE /\ fPhase' = "picked"
          \* Nothing is published; the step and the retired set stay as they are.
          /\ UNCHANGED <<pub, step, retired>>
  \* Nothing else changes.
  /\ UNCHANGED <<nextMt, ingested, compacted, rolled, round, sealVars, rollVars,
                 fTarget, fBase, fNext, verVars, replaced>>

\* The split bugs only: the flush retires its memtable in a publication of
\* its own, building a view whose frozen list lacks it, by identity
\* (retire_memtable's closure). The real code never reaches this step.
FlushRetireBuild ==
  \* The table is installed.
  /\ fPhase = "installed"
  \* It built from the view published now, without its memtable.
  /\ fBase' = pub
  \* The view it built: the published one without its memtable in the frozen list.
  /\ fNext' = [pub EXCEPT !.frozen = Without(pub.frozen, fTarget)]
  \* The flush has built its retire.
  /\ fPhase' = "retireBuilt"
  \* Nothing else changes.
  /\ UNCHANGED <<pub, nextMt, ingested, compacted, rolled, round, sealVars, rollVars,
                 fTarget, verVars, step, retired, replaced>>

\* The retire's swap: a memtable publish, so the PlainRetire bug stores it.
\* In, the flush is done; out, it builds again.
FlushRetireSwap ==
  \* It built its view.
  /\ fPhase = "retireBuilt"
  \* Does the swap go in? The bug PlainRetire stores without the compare-and-swap, so it always
  \* does.
  /\ IF Swaps(fBase, Mode = "PlainRetire")
     \* In: the memtable is gone from the frozen list, for good.
     THEN /\ pub' = fNext
          \* The memtable is recorded as retired.
          /\ retired' = retired \cup {fTarget}
          \* The flush is done.
          /\ fPhase' = "idle"
     \* Out: back to build on what won.
     ELSE /\ fPhase' = "installed"
          \* Nothing is published or retired.
          /\ UNCHANGED <<pub, retired>>
  \* Nothing else changes.
  /\ UNCHANGED <<nextMt, ingested, compacted, rolled, round, sealVars, rollVars,
                 fTarget, fBase, fNext, verVars, step, replaced>>

\* ---- Compaction and ingest (version publishes) ----------------------------

\* The view a compaction or an ingest builds from `v`: a compaction's
\* output holds the same data as its inputs, so the set of places data lives
\* in is the same; an ingest adds its table.
VersionResult(v, ingest) ==
  \* An ingest: `v` with the next ingested table added ...
  IF ingest
  \* (That view.)
  THEN [v EXCEPT !.tables = v.tables \cup {IngestTable(ingested + 1)}]
  \* ... a compaction: `v` as it is.
  ELSE v

\* A compaction or an ingest starts to install: its section enters the
\* version store's step and builds its view from the one published now.
VersionBuild ==
  \* Idle, and the step is free.
  /\ cPhase = "idle"
  \* (The step runs no other section.)
  /\ step = "none"
  \* It is an ingest, if one is left, or a compaction, if one is left.
  /\ \E ingest \in BOOLEAN :
       \* The kind chosen still has budget left.
       /\ IF ingest THEN ingested < MaxIngests ELSE compacted < MaxCompactions
       \* It remembers which kind it is.
       /\ cIngest' = ingest
       \* The view it built, from the published one.
       /\ cNext' = VersionResult(pub, ingest)
  \* Its section runs until its swap goes in.
  /\ step' = "version"
  \* It built from the view published now.
  /\ cBase' = pub
  \* It has built its view.
  /\ cPhase' = "built"
  \* Nothing else changes.
  /\ UNCHANGED <<pub, nextMt, ingested, compacted, rolled, round, sealVars, rollVars,
                 flushVars, retired, replaced>>

\* After a lost swap it builds again, from the view published now, its
\* section still the one in the step.
VersionRebuild ==
  \* Its swap lost.
  /\ cPhase = "retry"
  \* It builds again from the view published now.
  /\ cBase' = pub
  \* The same kind of change, applied to that view.
  /\ cNext' = VersionResult(pub, cIngest)
  \* It has built its view.
  /\ cPhase' = "built"
  \* Nothing else changes.
  /\ UNCHANGED <<pub, nextMt, ingested, compacted, rolled, round, sealVars, rollVars,
                 flushVars, cIngest, step, retired, replaced>>

\* The version publish's swap, which the PlainVersion bug stores. In, the
\* result is published and the section ends; out, it builds again.
VersionSwap ==
  \* It built its view.
  /\ cPhase = "built"
  \* Does the swap go in? The bug PlainVersion stores without the compare-and-swap, so it always
  \* does.
  /\ IF Swaps(cBase, Mode = "PlainVersion")
     \* In: published; count the ingest or the compaction.
     THEN /\ pub' = cNext
          \* An ingest is counted,
          /\ ingested' = IF cIngest THEN ingested + 1 ELSE ingested
          \* or a compaction is.
          /\ compacted' = IF cIngest THEN compacted ELSE compacted + 1
          \* The publisher is idle again.
          /\ cPhase' = "idle"
          \* The step is free.
          /\ step' = "none"
     \* Out: build again on what won.
     ELSE /\ cPhase' = "retry"
          \* Nothing is published or counted, and its section stays in the step.
          /\ UNCHANGED <<pub, ingested, compacted, step>>
  \* Nothing else changes.
  /\ UNCHANGED <<nextMt, rolled, round, sealVars, rollVars, flushVars,
                 cIngest, cBase, cNext, retired, replaced>>

\* Every step any publisher can take.
Next ==
  \* The rotation builds or swaps ...
  \/ SealBuild \/ SealSwap
  \* ... the rollback builds or swaps ...
  \/ RollbackBuild \/ RollbackSwap
  \* ... the flush picks, installs or retires ...
  \/ FlushPick \/ FlushInstallBuild \/ FlushInstallSwap \/ FlushRetireBuild \/ FlushRetireSwap
  \* ... or a compaction or an ingest builds, swaps or builds again.
  \/ VersionBuild \/ VersionSwap \/ VersionRebuild

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

\* ===========================================================================
\* THE RULES
\* ===========================================================================

\* Every variable holds what its comment says.
TypeOK ==
  \* The published view's active memtable is one that exists ...
  /\ pub.active \in 1..MaxMemtables
  \* ... and the counters stay in their bounds.
  /\ nextMt \in 2..(MaxMemtables + 1)
  \* The ingests stay within their budget.
  /\ ingested \in 0..MaxIngests
  \* The compactions stay within their budget.
  /\ compacted \in 0..MaxCompactions
  \* The rollbacks stay within their budget.
  /\ rolled \in 0..MaxRollbacks
  \* The round is free, a rotation's or a rollback's.
  /\ round \in {"none", "seal", "rollback"}
  \* The step runs nobody's section, the flush's or a version publisher's.
  /\ step \in {"none", "flush", "version"}

\* THE HEADLINE. Every memtable ever made holds its data in a published
\* place: it is the active one, a frozen one, or its table is in the
\* version; or a rollback replaced it, and then its copy is such a memtable
\* and the rule holds for the copy. Rules out the flush's plain store
\* publishing (active 2, frozen [], tables {1}) over (active 3,
\* frozen [1, 2]): memtable 3, with every write in it, is nowhere. Lean:
\* no_lost_memtable.
NoLostMemtable ==
  \* Every memtable made so far ...
  \A m \in 1..(nextMt - 1) :
    \* ... was replaced by its copy, or is active, frozen, or in a table of
    \* the version.
    m \in replaced \/ m = pub.active \/ m \in Range(pub.frozen) \/ m \in pub.tables

\* Every table an ingest installed is in the published version. Rules out
\* the story at the top: the rollback's plain store publishes (active 2,
\* frozen [], tables {}) over (active 1, frozen [], tables {101}), and the
\* ingested table is in no view. Lean: no_lost_ingest.
NoLostIngest ==
  \* Every ingest so far has its table in the version.
  \A n \in 1..ingested : IngestTable(n) \in pub.tables

\* A memtable's data is never in two published places at once: a frozen
\* memtable's table is not in the version. Rules out the split flush: its
\* view between the two publications held memtable 1 frozen and table 1
\* installed, and a read applied each of memtable 1's merge operands twice.
\* Lean: flush_keeps_sources_disjoint.
NoDoubleSource ==
  \* No frozen memtable ...
  \A m \in Range(pub.frozen) :
    \* ... has its table installed.
    m \notin pub.tables

\* A memtable a flush retired never comes back. Rules out a stale view
\* republishing a frozen list that still names it: the next flush would
\* write it again and install that old table above newer ones. Lean:
\* retired_stay_gone.
RetiredStayGone ==
  \* Every retired memtable is neither active nor frozen.
  \A m \in retired : m # pub.active /\ m \notin Range(pub.frozen)

\* A memtable a rollback replaced never comes back: it still holds the
\* commits the rollback discarded, whose callers were told they failed.
\* Rules out a rotation that runs beside the rollback: it seals memtable 1
\* while the rollback is replacing it, the rollback finds another memtable
\* active and leaves the view as it is, and memtable 1 is frozen, to be
\* flushed with the discarded commits in it. Lean: replaced_stay_gone.
ReplacedStayGone ==
  \* Every replaced memtable is not active, not frozen, and has no table.
  \A m \in replaced : m # pub.active /\ m \notin Range(pub.frozen) /\ m \notin pub.tables

\* The frozen list is in seal order, oldest first, which is the order
\* flushes install them in (LsmOrder.tla). Lean: frozen_sorted.
FrozenInSealOrder ==
  \* For every two places in the frozen list, the earlier one ...
  \A i \in 1..Len(pub.frozen) : \A j \in (i + 1)..Len(pub.frozen) :
    \* ... holds the memtable sealed first.
    pub.frozen[i] < pub.frozen[j]

====
