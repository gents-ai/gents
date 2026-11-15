--------------------------- MODULE TestCacheStore ---------------------------
(***************************************************************************)
(* The test-result store on one host, for one test and key (keys do not    *)
(* interact). Model.lean proves a skip sound when every record came from a *)
(* faithful execution; this spec covers what happens when that premise     *)
(* fails, under concurrency: slices of several runs write passes, a full   *)
(* run contradicts them, eviction removes records, and build jobs read the *)
(* store in several steps while all of that happens.                       *)
(*                                                                         *)
(* Each record is its own file, published by rename, so a reader sees a    *)
(* record whole or not at all, and writers never overwrite each other.     *)
(*                                                                         *)
(* Property: a reader that starts after a contradiction is durable never   *)
(* skips the test. It holds only if contradictions never expire: with      *)
(* ExpireContradictions, TLC finds a slow slice whose stale pass lands     *)
(* after the contradiction was removed and is skipped on again.            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS Writers, Readers, MaxSteps, ExpireContradictions

VARIABLES
    now,          \* step counter: orders durable events and reader starts
    pending,      \* writers holding a pass not yet published
    records,      \* writers whose record file is durable
    contradicted, \* the contradiction file is durable
    firstContradiction, \* ghost: step at which a contradiction first became durable, 0 if never
    found,        \* the full run has seen the test fail where a record vouched
    rpc, rstart, rseen, rdecision

vars == <<now, pending, records, contradicted, firstContradiction, found,
          rpc, rstart, rseen, rdecision>>

Init ==
    /\ now = 1
    /\ pending = Writers
    /\ records = {}
    /\ contradicted = FALSE
    /\ firstContradiction = 0
    /\ found = FALSE
    /\ rpc = [r \in Readers |-> "idle"]
    /\ rstart = [r \in Readers |-> 0]
    /\ rseen = [r \in Readers |-> {}]
    /\ rdecision = [r \in Readers |-> "none"]

Step == now' = now + 1

(* A slice publishes a pass it observed, possibly long after observing it. *)
Publish(w) ==
    /\ w \in pending
    /\ pending' = pending \ {w}
    /\ records' = records \cup {w}
    /\ Step
    /\ UNCHANGED <<contradicted, firstContradiction, found, rpc, rstart, rseen, rdecision>>

(* A full run executes the test, sees it fail where a record vouches, and   *)
(* publishes the contradiction.                                            *)
Contradict ==
    /\ ~found
    /\ records /= {}
    /\ found' = TRUE
    /\ contradicted' = TRUE
    /\ firstContradiction' = IF firstContradiction = 0 THEN now ELSE firstContradiction
    /\ Step
    /\ UNCHANGED <<pending, records, rpc, rstart, rseen, rdecision>>

(* Retention and size caps remove records at any time. *)
Evict(w) ==
    /\ w \in records
    /\ records' = records \ {w}
    /\ Step
    /\ UNCHANGED <<pending, contradicted, firstContradiction, found, rpc, rstart, rseen, rdecision>>

Expire ==
    /\ ExpireContradictions
    /\ contradicted
    /\ contradicted' = FALSE
    /\ Step
    /\ UNCHANGED <<pending, records, firstContradiction, found, rpc, rstart, rseen, rdecision>>

(* A build job looks the test up: it starts, lists the record files, reads  *)
(* each (an evicted one is gone), then reads the contradiction and decides. *)
Start(r) ==
    /\ rpc[r] = "idle"
    /\ rpc' = [rpc EXCEPT ![r] = "listing"]
    /\ rstart' = [rstart EXCEPT ![r] = now]
    /\ Step
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rseen, rdecision>>

List(r) ==
    /\ rpc[r] = "listing"
    /\ rseen' = [rseen EXCEPT ![r] = records]
    /\ rpc' = [rpc EXCEPT ![r] = "reading"]
    /\ Step
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rstart, rdecision>>

Read(r) ==
    /\ rpc[r] = "reading"
    /\ rseen' = [rseen EXCEPT ![r] = rseen[r] \cap records]
    /\ rpc' = [rpc EXCEPT ![r] = "checking"]
    /\ Step
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rstart, rdecision>>

Decide(r) ==
    /\ rpc[r] = "checking"
    /\ rdecision' = [rdecision EXCEPT ![r] =
                        IF rseen[r] /= {} /\ ~contradicted THEN "skip" ELSE "run"]
    /\ rpc' = [rpc EXCEPT ![r] = "done"]
    /\ Step
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rstart, rseen>>

Next ==
    \/ \E w \in Writers : Publish(w) \/ Evict(w)
    \/ Contradict
    \/ Expire
    \/ \E r \in Readers : Start(r) \/ List(r) \/ Read(r) \/ Decide(r)

Spec == Init /\ [][Next]_vars

StepBound == now <= MaxSteps

TypeOK ==
    /\ pending \subseteq Writers
    /\ records \subseteq Writers
    /\ contradicted \in BOOLEAN
    /\ rdecision \in [Readers -> {"none", "skip", "run"}]

(* A reader that started after the contradiction was durable runs the test. *)
ContradictionHonoured ==
    \A r \in Readers :
        (rdecision[r] = "skip") =>
            (firstContradiction = 0 \/ rstart[r] < firstContradiction)

(* A skip is backed by a record the reader read whole. *)
SkipHasRecord ==
    \A r \in Readers : (rdecision[r] = "skip") => rseen[r] /= {}
=============================================================================
