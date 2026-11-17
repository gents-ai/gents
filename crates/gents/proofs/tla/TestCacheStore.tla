---- MODULE TestCacheStore ----
\* lean: none
\* crates: none
(***************************************************************************)
(* The test-result stores, one per runner host, for one test and key (keys *)
(* do not interact). Proofs/Ci/TestCache.lean proves a skip sound when     *)
(* every record in the store it reads came from a faithful execution; this *)
(* spec covers what happens when that premise fails, under concurrency:    *)
(* slices of several runs write passes, a full run contradicts them,       *)
(* eviction removes records, and build jobs read a store in several steps  *)
(* while all of that happens.                                              *)
(*                                                                         *)
(* Each record is its own file, published by rename, so a reader sees a    *)
(* record whole or not at all, and writers never overwrite each other.     *)
(*                                                                         *)
(* A cached build may land on any host of its role: it reads and writes    *)
(* only that host's store, and Init leaves every writer's and reader's     *)
(* host free, so TLC explores every placement. Stores share nothing.       *)
(*                                                                         *)
(* Property: a reader that starts after a contradiction is durable in its  *)
(* own host's store never skips the test. It holds only if contradictions  *)
(* never expire: with ExpireContradictions, TLC finds a slow slice whose   *)
(* stale pass lands after the contradiction was removed and is skipped on  *)
(* again. A contradiction found on one host does not reach the others      *)
(* (FleetContradictionHonoured fails with two hosts). That is accepted:    *)
(* a contradiction refutes the premises (TestCache.lean                    *)
(* `contradiction_refutes_premises`), the run that finds it fails, and its *)
(* remedy, an exclusion in .config/test-cache.toml, makes the test run on  *)
(* every host; full runs on main land on every host and find their own.    *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS Hosts, Writers, Readers, MaxSteps, ExpireContradictions

VARIABLES
    now,          \* step counter: orders durable events and reader starts
    whost,        \* the host each writer's slice runs on
    rhost,        \* the host each reader's build job runs on
    pending,      \* writers holding a pass not yet published
    records,      \* per host: writers whose record file is durable there
    contradicted, \* per host: the contradiction file is durable there
    firstContradiction, \* ghost, per host: step at which a contradiction first became durable, 0 if never
    found,        \* hosts where a full run has seen the test fail where a record vouched
    rpc, rstart, rseen, rdecision

vars == <<now, whost, rhost, pending, records, contradicted, firstContradiction, found,
          rpc, rstart, rseen, rdecision>>

Init ==
    /\ now = 1
    /\ whost \in [Writers -> Hosts]
    /\ rhost \in [Readers -> Hosts]
    /\ pending = Writers
    /\ records = [h \in Hosts |-> {}]
    /\ contradicted = [h \in Hosts |-> FALSE]
    /\ firstContradiction = [h \in Hosts |-> 0]
    /\ found = {}
    /\ rpc = [r \in Readers |-> "idle"]
    /\ rstart = [r \in Readers |-> 0]
    /\ rseen = [r \in Readers |-> {}]
    /\ rdecision = [r \in Readers |-> "none"]

Step == now' = now + 1

Placed == UNCHANGED <<whost, rhost>>

(* A slice publishes a pass it observed, possibly long after observing it, *)
(* to the store of the host it ran on.                                     *)
Publish(w) ==
    /\ w \in pending
    /\ pending' = pending \ {w}
    /\ records' = [records EXCEPT ![whost[w]] = @ \cup {w}]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<contradicted, firstContradiction, found, rpc, rstart, rseen, rdecision>>

(* A full run on host h executes the test, sees it fail where a record of  *)
(* h's store vouches, and publishes the contradiction there.               *)
Contradict(h) ==
    /\ h \notin found
    /\ records[h] /= {}
    /\ found' = found \cup {h}
    /\ contradicted' = [contradicted EXCEPT ![h] = TRUE]
    /\ firstContradiction' = [firstContradiction EXCEPT ![h] = IF @ = 0 THEN now ELSE @]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<pending, records, rpc, rstart, rseen, rdecision>>

(* Retention and size caps remove records at any time. *)
Evict(h, w) ==
    /\ w \in records[h]
    /\ records' = [records EXCEPT ![h] = @ \ {w}]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<pending, contradicted, firstContradiction, found, rpc, rstart, rseen, rdecision>>

Expire(h) ==
    /\ ExpireContradictions
    /\ contradicted[h]
    /\ contradicted' = [contradicted EXCEPT ![h] = FALSE]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<pending, records, firstContradiction, found, rpc, rstart, rseen, rdecision>>

(* A build job looks the test up in its host's store: it starts, lists the *)
(* record files, reads each (an evicted one is gone), then reads the       *)
(* contradiction and decides.                                              *)
Start(r) ==
    /\ rpc[r] = "idle"
    /\ rpc' = [rpc EXCEPT ![r] = "listing"]
    /\ rstart' = [rstart EXCEPT ![r] = now]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rseen, rdecision>>

List(r) ==
    /\ rpc[r] = "listing"
    /\ rseen' = [rseen EXCEPT ![r] = records[rhost[r]]]
    /\ rpc' = [rpc EXCEPT ![r] = "reading"]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rstart, rdecision>>

Read(r) ==
    /\ rpc[r] = "reading"
    /\ rseen' = [rseen EXCEPT ![r] = rseen[r] \cap records[rhost[r]]]
    /\ rpc' = [rpc EXCEPT ![r] = "checking"]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rstart, rdecision>>

Decide(r) ==
    /\ rpc[r] = "checking"
    /\ rdecision' = [rdecision EXCEPT ![r] =
                        IF rseen[r] /= {} /\ ~contradicted[rhost[r]] THEN "skip" ELSE "run"]
    /\ rpc' = [rpc EXCEPT ![r] = "done"]
    /\ Step
    /\ Placed
    /\ UNCHANGED <<pending, records, contradicted, firstContradiction, found, rstart, rseen>>

Next ==
    \/ \E w \in Writers : Publish(w)
    \/ \E h \in Hosts : Contradict(h) \/ Expire(h) \/ \E w \in Writers : Evict(h, w)
    \/ \E r \in Readers : Start(r) \/ List(r) \/ Read(r) \/ Decide(r)

Spec == Init /\ [][Next]_vars

StepBound == now <= MaxSteps

TypeOK ==
    /\ whost \in [Writers -> Hosts]
    /\ rhost \in [Readers -> Hosts]
    /\ pending \subseteq Writers
    /\ records \in [Hosts -> SUBSET Writers]
    /\ contradicted \in [Hosts -> BOOLEAN]
    /\ found \subseteq Hosts
    /\ rdecision \in [Readers -> {"none", "skip", "run"}]

(* A reader that started after a contradiction was durable in its host's   *)
(* store runs the test.                                                    *)
ContradictionHonoured ==
    \A r \in Readers :
        (rdecision[r] = "skip") =>
            (firstContradiction[rhost[r]] = 0 \/ rstart[r] < firstContradiction[rhost[r]])

(* The stronger property separate stores give up: a contradiction on any   *)
(* host stops skips on every host.                                         *)
FleetContradictionHonoured ==
    \A r \in Readers, h \in Hosts :
        (rdecision[r] = "skip") =>
            (firstContradiction[h] = 0 \/ rstart[r] < firstContradiction[h])

(* A skip is backed by a record the reader read whole from its own host's  *)
(* store, written by a slice that ran on that host.                        *)
SkipHasRecord ==
    \A r \in Readers :
        (rdecision[r] = "skip") =>
            (rseen[r] /= {} /\ \A w \in rseen[r] : whost[w] = rhost[r])
=============================================================================
