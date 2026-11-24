---- MODULE BatchRead ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A "batch read" (multi_get) asks for several keys at once and promises
\* that all the answers describe the same moment.
\*
\* The database's data lives in a memtable (in memory) and in files. A
\* "view" is one consistent list of where everything is right now: this
\* memtable, these files. Flushes keep replacing the current view with a new
\* one: the memtable is written to a file and a fresh memtable takes over.
\* A flush may also leave out an old version of a key when a newer version
\* of the same key exists and no registered snapshot needs the old one. A
\* batch read registers no snapshot.
\*
\* Tiny example of what can go wrong. The batch asks for keys 1 and 2, and
\* may see versions up to sequence 2; key 2 holds the version of sequence 2.
\* The batch answers key 1. Then a writer writes key 2 again (sequence 3)
\* and a flush writes the memtable out, keeping only the version of sequence
\* 3 for key 2. If the batch now looks up key 2 in the NEW view, it finds
\* only sequence 3, which it may not see, and answers "absent": wrong. If it
\* looks key 2 up in the view it loaded at the start, that view still holds
\* the old memtable, with the version of sequence 2 in it.
\*
\* What this model shows: resolving every key of the batch against the one
\* view loaded at the start gives every key the right answer, whatever
\* writes and flushes happen in between; resolving each key against the
\* then-current view does not (E6).
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/BatchView.lean:
\*   batch_one_view_point_in_time   a batch resolved against the view it
\*                                  captured answers every key as of its start
\*   batch_one_view_newest          each answer is the newest version the
\*                                  snapshot may see (with read_newest from
\*                                  LsmOrder.lean)
\*   batch_one_view_absent          each absent answer is right
\*   view_per_key_breaks_batch      the RED case, as a counterexample
\* TLC checks the same invariants here over every interleaving of writes,
\* flushes and the batch's key-by-key resolution.
\*
\* THE ENGINE.
\*   RegolithEngine::multi_get_latest   src/engine/mod.rs
\*     Loads the published view, then samples the read horizon as the
\*     batch's snapshot sequence. The batch registers no snapshot.
\*   RegolithEngine::multi_get_in_view  src/engine/mod.rs
\*     Resolves every key against that one view. With a merge operator it
\*     goes key by key through get_with_merge, which is handed the same
\*     view. (Before E6, get_with_merge loaded the current view again for
\*     each key: the defect below.)
\*   Flush (rotate_memtable and the flush that follows)
\*     Writes the memtable out as an L0 file. With no registered snapshot
\*     that needs it, a version shadowed by a newer one of the same key is
\*     not written.
\*
\* THE DEFECT (Mode = "ViewPerKey"). Between two keys of the batch, a write
\* lands a newer version of a later key and a flush writes the memtable out
\* without the version the batch's snapshot needs. The later key, resolved
\* against the new view, reads absent or older.
\*
\* THE FIX (Mode = "OneView"). Every key of the batch resolves against the
\* view captured at the start. That view holds its memtable by reference, so
\* the versions the snapshot needs stay reachable through it whatever later
\* flushes write.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - A write is one step: sequence, memtable insert and publication.
\*   - The batch captures its view and samples its snapshot in one step. In
\*     the engine a write may land between the two; the read linearizes at
\*     the view load (the argument on RegolithEngine::get_latest), so that
\*     write is not part of the batch either way.
\*   - A flush goes straight to the new view; the intermediate view holding
\*     the frozen memtable answers the same as the one before it.
\*   - The flushed files are one set of versions, newest-first order inside
\*     it being decided by sequence (LsmOrder.tla covers the L0 order).
\*
\* CONFIGURATIONS.
\*   MC_BatchRead_Green                 Mode = "OneView"
\*     BatchConsistent and BatchExact hold.
\*   MC_BatchRead_Red_ViewPerKey        BatchConsistent fails.
\*   MC_BatchRead_Red_ViewPerKey_Exact  BatchExact fails: the second view
\*                                      changes an answer, not only its source.

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The keys writers write, as numbers.
  Keys,
  \* The keys the batch reads, in the order it reads them: a list over Keys (the `keys` argument of
  \* multi_get).
  BatchKeys,
  \* The last sequence number a write may take. It bounds how many writes a run of the model makes.
  MaxSeq,
  \* The largest number of flushes.
  MaxFlush,
  \* "OneView" (the engine: every key of the batch is resolved against one view) or "ViewPerKey"
  \* (the planted bug, E6).
  Mode

\* There is at least one key, and keys are numbers.
ASSUME Keys \subseteq Nat /\ Keys # {}
\* The batch reads at least one key, each a known key.
ASSUME BatchKeys \in Seq(Keys) /\ Len(BatchKeys) > 0
\* The two bounds are numbers.
ASSUME MaxSeq \in Nat /\ MaxFlush \in Nat
\* The mode is one of the two.
ASSUME Mode \in {"OneView", "ViewPerKey"}

\* The memtable ids: one per flush, plus the first.
MemIds == 0..MaxFlush

\* The batch the configurations read (a .cfg file cannot spell a sequence):
\* key 1, key 2, then key 1 again, since multi_get accepts duplicates.
ReadOneTwoOne == <<1, 2, 1>>

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The last sequence number handed out, 0 before the first write (the engine's commit sequence;
  \* what readers may see of it is RegolithEngine::visible_seq).
  seq,
  \* Every memtable there ever is, by id: each a set of versions <<key, sequence>> (MemTable,
  \* src/engine/memtable.rs). A view holds a memtable by id, as the engine's view holds an
  \* Arc<MemTable>, so it sees writes that land in that memtable later.
  mems,
  \* The id of the memtable that takes writes now (ReadView::active).
  active,
  \* The views published so far, oldest first; each is <<the id of its memtable, the versions
  \* already flushed to files>>. The last one is the current view (ReadView,
  \* src/engine/read_view.rs; RegolithEngine::view holds the current one).
  views,
  \* Every version ever written. Model bookkeeping (a "ghost"): flushes never thin it, so it is the
  \* truth the answers are compared with.
  history,
  \* Where the batch read is: "idle" (not begun), "reading" or "done".
  bstate,
  \* The batch's snapshot sequence: it may see versions up to this one (the `snapshot_seq` of
  \* multi_get_in_view).
  bsnap,
  \* Which view the batch loaded when it began, as a position in `views` (the `view` argument of
  \* multi_get_in_view).
  bview,
  \* The position in BatchKeys of the next key to resolve.
  bnext,
  \* The positions of the views that keys were resolved against so far. Model bookkeeping for the
  \* headline promise.
  bused,
  \* The answer given for each key resolved so far, as a sequence number (0 for "absent"); the
  \* `results` of multi_get_in_view.
  bout

\* Every variable, so a step that changes none of them is a stutter.
vars == <<seq, mems, active, views, history, bstate, bsnap, bview, bnext, bused, bout>>

\* The greatest element of a finite, nonempty set of naturals.
Max(S) == CHOOSE m \in S : \A n \in S : n <= m

\* The sequences of the versions of key k in a set of versions F that a
\* snapshot at s may see.
Visible(F, k, s) == {v[2] : v \in {x \in F : x[1] = k /\ x[2] <= s}}

\* The newest of those, or 0 for absent (sequences start at 1).
NewestIn(F, k, s) == IF Visible(F, k, s) = {} THEN 0 ELSE Max(Visible(F, k, s))

\* A point read of key k at snapshot s through view `view`: the memtable
\* first, then the flushed versions. The memtable is read as it is now, which
\* is how the engine's view sees its memtable.
Resolve(view, k, s) ==
  \* Does the view's memtable hold a version of k the snapshot may see?
  IF Visible(mems[view[1]], k, s) # {}
    \* Yes: the newest such version in the memtable.
    THEN NewestIn(mems[view[1]], k, s)
    \* No: the newest such version among the flushed ones.
    ELSE NewestIn(view[2], k, s)

\* What a flush writes: each key's newest version only, as when no
\* registered snapshot needs an older one.
NewestOnly(F) == {v \in F : ~\E w \in F : w[1] = v[1] /\ w[2] > v[2]}

\* The position in views of the current view: the last one published.
Current == Len(views)

\* The start: no write, one view over an empty memtable, the batch not begun.
Init ==
  \* No write yet.
  /\ seq     = 0
  \* Every memtable is empty.
  /\ mems    = [m \in MemIds |-> {}]
  \* Memtable 0 takes the writes.
  /\ active  = 0
  \* One view is published: memtable 0 over no flushed versions.
  /\ views   = <<<<0, {}>>>>
  \* Nothing was ever written.
  /\ history = {}
  \* The batch has not begun.
  /\ bstate  = "idle"
  \* It has no snapshot yet.
  /\ bsnap   = 0
  \* It has loaded no view yet.
  /\ bview   = 0
  \* Its next key is the first.
  /\ bnext   = 1
  \* It has used no view.
  /\ bused   = {}
  \* It has no answers.
  /\ bout    = [i \in 1..Len(BatchKeys) |-> 0]

\* A write of key k: the next sequence, into the active memtable, published.
Put(k) ==
  \* The model may still write.
  /\ seq < MaxSeq
  \* The write takes the next sequence.
  /\ seq'     = seq + 1
  \* Its version goes into the active memtable.
  /\ mems'    = [mems EXCEPT ![active] = @ \cup {<<k, seq + 1>>}]
  \* And into the record of everything ever written.
  /\ history' = history \cup {<<k, seq + 1>>}
  \* Nothing else changes.
  /\ UNCHANGED <<active, views, bstate, bsnap, bview, bnext, bused, bout>>

\* A flush: the active memtable is written out (its newest versions only)
\* and a new view publishes a fresh memtable over the flushed versions.
Flush ==
  \* One more flush is allowed.
  /\ active < MaxFlush
  \* There is something to flush.
  /\ mems[active] # {}
  \* A fresh memtable takes the writes from now on.
  /\ active' = active + 1
  \* A new view is published: the fresh memtable over the versions flushed so far plus the newest
  \* version of each key in the old memtable.
  /\ views'  = Append(views, <<active + 1, views[Current][2] \cup NewestOnly(mems[active])>>)
  \* Nothing else changes. The old memtable keeps its versions, for any view that still holds it.
  /\ UNCHANGED <<seq, mems, history, bstate, bsnap, bview, bnext, bused, bout>>

\* multi_get_latest: the batch captures the current view and takes the
\* newest published sequence as its snapshot.
Begin ==
  \* The batch has not begun.
  /\ bstate = "idle"
  \* It is reading now.
  /\ bstate' = "reading"
  \* Its snapshot is the newest sequence.
  /\ bsnap'  = seq
  \* It loads the current view.
  /\ bview'  = Current
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mems, active, views, history, bnext, bused, bout>>

\* Resolve the next key: against the captured view (the fix), or against
\* the view current at this moment (the defect).
ResolveNext ==
  \* at: the view this key is resolved against: the one loaded at the start (the engine), or
  \* whichever is current now (the planted bug).
  LET at == IF Mode = "OneView" THEN bview ELSE Current
      \* k: the next key of the batch.
      k  == BatchKeys[bnext]
  \* The batch is reading.
  IN /\ bstate = "reading"
     \* The key's answer is a point read of k through that view at the batch's snapshot.
     /\ bout'   = [bout EXCEPT ![bnext] = Resolve(views[at], k, bsnap)]
     \* That view is noted as used.
     /\ bused'  = bused \cup {at}
     \* The batch moves to its next key.
     /\ bnext'  = bnext + 1
     \* After the last key the batch is done.
     /\ bstate' = IF bnext = Len(BatchKeys) THEN "done" ELSE "reading"
     \* Nothing else changes.
     /\ UNCHANGED <<seq, mems, active, views, history, bsnap, bview>>

\* Every step the system can take.
Next ==
  \* A key is written.
  \/ \E k \in Keys : Put(k)
  \* The memtable is flushed.
  \/ Flush
  \* The batch begins.
  \/ Begin
  \* The batch resolves its next key.
  \/ ResolveNext

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The sequence stays within the bound.
  /\ seq \in 0..MaxSeq
  \* The active memtable is a known id.
  /\ active \in MemIds
  \* There is one view per memtable so far.
  /\ Len(views) = active + 1
  \* The batch is in one of its three states.
  /\ bstate \in {"idle", "reading", "done"}
  \* Its next position is within the batch, or one past the end.
  /\ bnext \in 1..(Len(BatchKeys) + 1)
  \* The views it used are published views.
  /\ bused \subseteq 1..Len(views)
  \* Everything ever written is a known key at a sequence within the bound.
  /\ history \subseteq Keys \X (1..MaxSeq)

\* THE HEADLINE. Every key of the batch resolves against the same view.
\* Rules out, for example: the batch resolves key 1 against the view it loaded, a flush publishes a
\* new view, and the batch resolves key 2 against that new view. The two answers would then describe
\* two different moments.
BatchConsistent == Cardinality(bused) <= 1

\* What that buys: every key resolved so far answers the newest version of
\* that key the batch's snapshot may see, as of the batch's start.
\* Lean: batch_one_view_point_in_time, batch_one_view_newest.
BatchExact ==
  \* For every key resolved so far: its answer is the newest version of that key, among everything
  \* ever written, that the batch's snapshot may see. Rules out, for example: the batch began when
  \* key 2 held the version of sequence 1; a newer write and a flush follow; and the batch answers
  \* "absent" for key 2 because the flush no longer wrote the old version out.
  \A i \in 1..(bnext - 1) : bout[i] = NewestIn(history, BatchKeys[i], bsnap)

====
