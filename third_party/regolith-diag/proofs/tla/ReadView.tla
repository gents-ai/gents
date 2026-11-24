---- MODULE ReadView ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* Every read in regolith starts by asking "which memtables and which table
\* files make up the database right now?". The answer is one object, the
\* read view (ReadView in src/engine/read_view.rs), and the database keeps
\* exactly one current view in one cell (ReadViewCell, a kovan Atom).
\*
\* Readers must not lock anything to get it. So:
\*   - a reader "pins" itself (kovan::pin: it tells the reclaimer "I am
\*     reading, do not free what I might be looking at"), then loads the
\*     cell's pointer, uses the view, and unpins (drops its ViewGuard);
\*   - a writer that changes the view (a memtable rotation, a flush, a
\*     compaction, an ingest) builds a whole new view from the current one
\*     and swaps it in with one compare-and-swap (CAS). If another writer
\*     got in first, the CAS fails and it builds again on the newer view;
\*   - the view it replaced is "retired": handed to the reclaimer, which
\*     frees it only once every reader that might be looking at it has
\*     unpinned.
\*
\* Tiny example. Reader R pins, then loads view 1. Writer W builds view 2
\* from view 1 and swaps it in, retiring view 1. R is still reading view 1,
\* so the reclaimer must not free view 1 yet: it waits for R. When R
\* unpins, view 1 may go.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - the reclaimer frees a view a reader is still using (NoFreedRead);
\*   - a reader gets a view older than one that was already published when
\*     its load began (FreshLoad);
\*   - two writers race and one of them overwrites the other's change, so
\*     a rotation or a flush silently disappears (ChainOfPublications).
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   published   ReadViewCell::current, the Atom holding the current view
\*   made        how many views exist; views are numbered in the order
\*               they are built, so a bigger number is a newer view
\*   retired     views a publication replaced and kovan was given
\*   freed       views kovan has given the memory of back
\*   waitFor     for each retired view, the readers kovan still waits for:
\*               the readers that were pinned when it was retired
\*   rstate      a reader's progress: idle, pinned (ViewGuard being made),
\*               holding (ViewGuard alive)
\*   pstate      a publisher's progress inside ReadViewCell::publish: idle,
\*               or built (it loaded the current view and built the next)
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - What a view contains. Only which view a reader holds matters here;
\*     LsmOrder.tla and BatchRead.tla check what a read finds inside one.
\*   - kovan's epochs and batches. The model keeps only the rule they
\*     implement: a retired view waits for the readers pinned when it was
\*     retired. tests/loom_read_view.rs checks the memory ordering of the
\*     same rule, and kovan's own suite checks kovan.
\*   - The padding regolith adds so a retired view is placed at once
\*     (engine::reclaim). It changes when a view is freed, never whether.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_ReadView_Green            the real protocol: every invariant holds.
\*   MC_ReadView_Red_FreeEarly    the reclaimer frees a view the moment it
\*                                is retired: NoFreedRead breaks.
\*   MC_ReadView_Red_StaleLoad    a reader reuses the view it held last
\*                                time instead of loading: FreshLoad breaks.
\*   MC_ReadView_Red_PlainStore   a publisher stores its view without the
\*                                CAS: ChainOfPublications breaks.
\*
\* Lean, proofs/lean/Regolith/ReadView.lean, proves the same rules for every
\* number of readers and publications (reachable_safe, cas_chain).

\* Numbers, sequences (for the publication history) and finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The reader threads.
  Readers,
  \* The publishing threads (rotation, flush, compaction, ingest).
  Publishers,
  \* The most views that may ever be built, which bounds the model.
  MaxViews,
  \* "Green" for the real code, or the name of one planted bug.
  Mode

\* The bug names this model knows.
ASSUME Mode \in {"Green", "FreeEarly", "StaleLoad", "PlainStore"}
\* At least the first view exists.
ASSUME MaxViews \in Nat /\ MaxViews >= 1

\* View numbers. 0 means "no view".
Views == 1..MaxViews

\* The state that changes from step to step.
VARIABLES
  \* The view the cell holds now (ReadViewCell::current).
  published,
  \* How many views have been built so far: views 1..made exist.
  made,
  \* The views a publication replaced (handed to kovan's retire).
  retired,
  \* The views whose memory kovan has freed.
  freed,
  \* [Views -> SUBSET Readers]: who kovan still waits for, per retired view.
  waitFor,
  \* [Readers -> {"idle", "pinned", "holding"}]: each reader's progress.
  rstate,
  \* [Readers -> 0..MaxViews]: the view each reader holds (0 = none).
  rview,
  \* [Readers -> 0..MaxViews]: what was published when the reader pinned.
  \* A ghost: the code never stores it; it is what FreshLoad compares to.
  rfloor,
  \* [Readers -> 0..MaxViews]: the view the reader held last time. Only
  \* the StaleLoad bug ever uses it.
  rlast,
  \* [Publishers -> {"idle", "built"}]: each publisher's progress.
  pstate,
  \* [Publishers -> 0..MaxViews]: the view a publisher built its next from.
  pbase,
  \* A ghost history: one record <<built from, new view, replaced view>>
  \* per publication that went in.
  chain

\* Every variable, so a step that changes none of them is a stutter.
vars == <<published, made, retired, freed, waitFor, rstate, rview, rfloor,
          rlast, pstate, pbase, chain>>

\* The start: the engine opened with view 1, nobody reading or writing.
Init ==
  \* The cell holds view 1, the view open built.
  /\ published = 1
  \* Only view 1 exists.
  /\ made = 1
  \* Nothing has been replaced yet ...
  /\ retired = {}
  \* ... so nothing has been freed.
  /\ freed = {}
  \* Nobody is waited for.
  /\ waitFor = [v \in Views |-> {}]
  \* Every reader is idle ...
  /\ rstate = [r \in Readers |-> "idle"]
  \* ... holding no view ...
  /\ rview = [r \in Readers |-> 0]
  \* ... with no floor noted yet ...
  /\ rfloor = [r \in Readers |-> 0]
  \* ... and no view held before.
  /\ rlast = [r \in Readers |-> 0]
  \* Every publisher is idle ...
  /\ pstate = [p \in Publishers |-> "idle"]
  \* ... having built from nothing yet.
  /\ pbase = [p \in Publishers |-> 0]
  \* No publication has happened.
  /\ chain = <<>>

\* A reader starts a read: it pins (kovan::pin inside ReadViewCell::load).
\* From here kovan counts it as a reader that may hold whatever it loads.
Pin(r) ==
  \* Only an idle reader starts a new read.
  /\ rstate[r] = "idle"
  \* It is now pinned.
  /\ rstate' = [rstate EXCEPT ![r] = "pinned"]
  \* Note what was published as its load begins (the ghost FreshLoad uses).
  /\ rfloor' = [rfloor EXCEPT ![r] = published]
  \* Nothing else changes.
  /\ UNCHANGED <<published, made, retired, freed, waitFor, rview, rlast,
                 pstate, pbase, chain>>

\* The pinned reader loads the cell's pointer (Atom::load): it now holds
\* that view. The StaleLoad bug instead reuses the view it held last time.
Load(r) ==
  \* Only a pinned reader loads.
  /\ rstate[r] = "pinned"
  \* The real load reads the cell; the bug keeps an old view when it has one.
  /\ rview' = [rview EXCEPT ![r] =
                 IF Mode = "StaleLoad" /\ rlast[r] # 0 THEN rlast[r] ELSE published]
  \* It holds a view now: a ViewGuard is alive.
  /\ rstate' = [rstate EXCEPT ![r] = "holding"]
  \* Nothing else changes.
  /\ UNCHANGED <<published, made, retired, freed, waitFor, rfloor, rlast,
                 pstate, pbase, chain>>

\* The reader is done and drops its ViewGuard: kovan stops waiting for it.
Unpin(r) ==
  \* Only a reader that holds a view lets go of one.
  /\ rstate[r] = "holding"
  \* It is idle again ...
  /\ rstate' = [rstate EXCEPT ![r] = "idle"]
  \* ... holds nothing ...
  /\ rview' = [rview EXCEPT ![r] = 0]
  \* ... and remembers what it held (only the StaleLoad bug looks).
  /\ rlast' = [rlast EXCEPT ![r] = rview[r]]
  \* No retired view waits for this reader any more.
  /\ waitFor' = [v \in Views |-> waitFor[v] \ {r}]
  \* Nothing else changes.
  /\ UNCHANGED <<published, made, retired, freed, rfloor, pstate, pbase, chain>>

\* A publisher loads the current view and builds the next one from it
\* (the first half of one attempt of ReadViewCell::publish).
Build(p) ==
  \* An idle publisher starts an attempt ...
  /\ pstate[p] = "idle"
  \* ... if the model may still build a view.
  /\ made < MaxViews
  \* It remembers the view it built from.
  /\ pbase' = [pbase EXCEPT ![p] = published]
  \* It holds a built view, ready to swap in.
  /\ pstate' = [pstate EXCEPT ![p] = "built"]
  \* Nothing else changes yet.
  /\ UNCHANGED <<published, made, retired, freed, waitFor, rstate, rview,
                 rfloor, rlast, chain>>

\* The publisher's compare-and-swap (Atom::compare_and_swap). It goes in
\* only if the cell still holds the view it built from; otherwise it fails
\* and the publisher goes back to build again on what won. The PlainStore
\* bug swaps in whatever is there.
Swap(p) ==
  \* Only a publisher holding a built view swaps.
  /\ pstate[p] = "built"
  \* Does the swap go in? Yes when the cell still holds the view this one was built from; the bug
  \* PlainStore always says yes.
  /\ IF Mode = "PlainStore" \/ published = pbase[p]
       \* The swap goes in.
       THEN
            \* The cell holds a new view ...
            /\ published' = made + 1
            \* ... which takes the next number.
            /\ made' = made + 1
            \* The view it replaced is retired ...
            /\ retired' = retired \cup {published}
            \* ... and waits for every reader pinned right now: any of
            \* them may be holding it or about to.
            /\ waitFor' = [waitFor EXCEPT ![published] =
                             {r \in Readers : rstate[r] # "idle"}]
            \* The FreeEarly bug frees it at once instead.
            /\ freed' = IF Mode = "FreeEarly" THEN freed \cup {published} ELSE freed
            \* Record what it was built from, what it is, what it replaced.
            /\ chain' = Append(chain, <<pbase[p], made + 1, published>>)
       \* The CAS failed: someone else published first.
       ELSE
            \* Nothing moves.
            UNCHANGED <<published, made, retired, freed, waitFor, chain>>
  \* Either way the attempt is over; a failed one will be built again.
  /\ pstate' = [pstate EXCEPT ![p] = "idle"]
  \* Readers are not touched by a publication.
  /\ UNCHANGED <<rstate, rview, rfloor, rlast, pbase>>

\* kovan frees a retired view once nobody it waits for is left.
Free(v) ==
  \* Only a retired view, not yet freed ...
  /\ v \in retired \ freed
  \* ... with nobody left to wait for.
  /\ waitFor[v] = {}
  \* Its memory goes back.
  /\ freed' = freed \cup {v}
  \* Nothing else changes.
  /\ UNCHANGED <<published, made, retired, waitFor, rstate, rview, rfloor,
                 rlast, pstate, pbase, chain>>

\* Every step the system can take.
Next ==
  \* Some reader pins, loads or unpins ...
  \/ \E r \in Readers : Pin(r) \/ Load(r) \/ Unpin(r)
  \* ... or some publisher builds or swaps ...
  \/ \E p \in Publishers : Build(p) \/ Swap(p)
  \* ... or the reclaimer frees a view.
  \/ \E v \in Views : Free(v)

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The cell holds a real view.
  /\ published \in Views
  \* The count of views built stays within the model's bound.
  /\ made \in Views
  \* Only views are retired ...
  /\ retired \subseteq Views
  \* ... or freed.
  /\ freed \subseteq Views
  \* Every reader is in one of its three states ...
  /\ rstate \in [Readers -> {"idle", "pinned", "holding"}]
  \* ... holding a view number or none.
  /\ rview \in [Readers -> 0..MaxViews]
  \* Every publisher is idle or holds a built view.
  /\ pstate \in [Publishers -> {"idle", "built"}]

\* THE HEADLINE. A reader never holds a view whose memory was freed.
\* Rules out: R loads view 1, W replaces it, the reclaimer frees view 1
\* while R still reads its memtables. Why the program needs it: a freed
\* view's memtables and table descriptors are gone, and reading them is a
\* use-after-free. Lean: reachable_safe.
NoFreedRead ==
  \* For every reader: if it holds a view, that view is not freed.
  \A r \in Readers : rstate[r] = "holding" => rview[r] \notin freed

\* A reader never gets a view older than one published before its load
\* began. Rules out: a flush published view 2 before R started, and R
\* still reads view 1, missing the table the flush wrote. Lean:
\* reachable_safe (its freshness half).
FreshLoad ==
  \* For every reader: if it holds a view, it is at least its floor.
  \A r \in Readers : rstate[r] = "holding" => rview[r] >= rfloor[r]

\* Every publication was built from exactly the view it replaced, so no
\* publication overwrote another's change. Rules out: a rotation and a
\* flush both build from view 1; the flush swaps in view 2, then the
\* rotation swaps in view 3, built from view 1, and the flush's change is
\* lost. Lean: cas_chain.
ChainOfPublications ==
  \* For every publication that went in: built from = replaced.
  \A i \in 1..Len(chain) : chain[i][1] = chain[i][3]

====
