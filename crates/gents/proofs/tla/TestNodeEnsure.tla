---- MODULE TestNodeEnsure ----
\* lean: none
\* crates: gents-documents
(***************************************************************************)
(* Concurrent callers of `ensure_test_node(node, node_did)` in             *)
(* `crates/gents-documents/src/test_support.rs` sharing one DefraDB node   *)
(* and `node_did`, optionally racing with an external writer (such as      *)
(* `enroll_test_user` or `bootstrap_signed_test_node`) that creates the    *)
(* same unique `Node` document without taking `ensure_test_node`'s lock.   *)
(*                                                                         *)
(* `Node.node_did` carries `@index(unique: true)`. Two callers that both   *)
(* observe `load_node == None` and open concurrent write transactions each *)
(* stage `create_Node`. Once one commits, the other's commit fails with a  *)
(* transaction conflict (retried up to `MaxAttempts` by `transact_write`)  *)
(* or a non-retryable unique-index rejection.                              *)
(*                                                                         *)
(* Safeguards:                                                             *)
(*   1. `Serialize` + `RecheckUnderLock`: per-(node, node_did) mutex       *)
(*      serializes `ensure_test_node` callers and rechecks `load_node`     *)
(*      before opening a write transaction.                                *)
(*   2. `AcceptConcurrentCreate`: if `transact_write` returns `Err`,       *)
(*      recheck `load_node`; succeed iff the `Node` now exists. Real store *)
(*      faults when `nodeDoc = "none"` still fail (`store_fault`) and are  *)
(*      never masked (`BlindIgnoreError = FALSE`).                         *)
(*                                                                         *)
(* GREEN (`MCTestNodeEnsure`): all safeguards enabled with both external   *)
(* writers and real store faults enabled; every invariant holds.           *)
(* RED (`MCTestNodeEnsureUnserialized`): code before the fix; two callers  *)
(* race to create `Node` and violate `NoConcurrencyPanic`.                 *)
(* RED (`MCTestNodeEnsureExternalRace`): serialized without concurrent-    *)
(* create acceptance; an external writer commits `Node` in flight and      *)
(* violates `NoConcurrencyPanic`.                                          *)
(* RED (`MCTestNodeEnsureBlindIgnore`): ignoring transaction errors        *)
(* without verifying `load_node` hides a real store fault and violates     *)
(* `AllDoneObserveSameNode`.                                               *)
(***************************************************************************)

EXTENDS Naturals

CONSTANTS
  Callers,                \* concurrent `ensure_test_node` callers
  MaxAttempts,            \* `transact_write` max attempts for TransactionConflict
  Serialize,              \* acquire per-(node, node_did) mutex around write
  RecheckUnderLock,       \* re-read `load_node` after acquiring mutex
  AcceptConcurrentCreate, \* on write error, recheck `load_node` and accept existing Node
  BlindIgnoreError,       \* broken safeguard: ignore write error without rechecking `load_node`
  AllowExternalWriter,    \* external writer may create the same Node outside the mutex
  AllowStoreFault         \* real non-concurrency store failure when Node is still absent

None == "none"
External == "ext"
Docs == Callers \cup {External}

VARIABLES
  nodeDoc,      \* None or the single committed Node doc ID in DefraDB
  nodeLock,     \* None or the caller holding the per-(node, node_did) mutex
  pc,           \* per caller program counter
  snap,         \* per caller MVCC snapshot of `nodeDoc` at `BeginTxn`
  attempts,     \* per caller transaction attempt count (0..MaxAttempts)
  observedDoc   \* per caller doc ID observed on completion

vars == <<nodeDoc, nodeLock, pc, snap, attempts, observedDoc>>

TypeOK ==
  /\ nodeDoc \in Docs \cup {None}
  /\ nodeLock \in Callers \cup {None}
  /\ pc \in [Callers -> {"fast_read", "wait_lock", "recheck", "begin_txn",
                         "txn_commit", "recheck_after_err", "done",
                         "panicked", "store_fault"}]
  /\ snap \in [Callers -> Docs \cup {None}]
  /\ attempts \in [Callers -> 0..MaxAttempts]
  /\ observedDoc \in [Callers -> Docs \cup {None}]

Init ==
  /\ nodeDoc = None
  /\ nodeLock = None
  /\ pc = [c \in Callers |-> "fast_read"]
  /\ snap = [c \in Callers |-> None]
  /\ attempts = [c \in Callers |-> 0]
  /\ observedDoc = [c \in Callers |-> None]

ReleaseLock(c) ==
  IF nodeLock = c THEN None ELSE nodeLock

\* Initial `load_node(node, node_did)` outside any lock.
FastRead(c) ==
  /\ pc[c] = "fast_read"
  /\ IF nodeDoc # None
     THEN /\ pc' = [pc EXCEPT ![c] = "done"]
          /\ observedDoc' = [observedDoc EXCEPT ![c] = nodeDoc]
     ELSE /\ pc' = [pc EXCEPT ![c] = IF Serialize THEN "wait_lock" ELSE "begin_txn"]
          /\ UNCHANGED observedDoc
  /\ UNCHANGED <<nodeDoc, nodeLock, snap, attempts>>

\* Acquire per-(node, node_did) mutex.
AcquireLock(c) ==
  /\ pc[c] = "wait_lock"
  /\ nodeLock = None
  /\ nodeLock' = c
  /\ pc' = [pc EXCEPT ![c] = IF RecheckUnderLock THEN "recheck" ELSE "begin_txn"]
  /\ UNCHANGED <<nodeDoc, snap, attempts, observedDoc>>

\* Double-checked `load_node(node, node_did)` after acquiring the mutex.
Recheck(c) ==
  /\ pc[c] = "recheck"
  /\ IF nodeDoc # None
     THEN /\ nodeLock' = ReleaseLock(c)
          /\ pc' = [pc EXCEPT ![c] = "done"]
          /\ observedDoc' = [observedDoc EXCEPT ![c] = nodeDoc]
     ELSE /\ pc' = [pc EXCEPT ![c] = "begin_txn"]
          /\ UNCHANGED <<nodeLock, observedDoc>>
  /\ UNCHANGED <<nodeDoc, snap, attempts>>

\* Open MVCC write transaction, capturing `nodeDoc` snapshot.
BeginTxn(c) ==
  /\ pc[c] = "begin_txn"
  /\ attempts[c] < MaxAttempts
  /\ snap' = [snap EXCEPT ![c] = nodeDoc]
  /\ attempts' = [attempts EXCEPT ![c] = attempts[c] + 1]
  /\ pc' = [pc EXCEPT ![c] = "txn_commit"]
  /\ UNCHANGED <<nodeDoc, nodeLock, observedDoc>>

\* Handle `transact_write` returning `Err` due to concurrency (Node already exists).
ConcurrencyWriteFailed(c) ==
  /\ IF BlindIgnoreError
     THEN /\ nodeLock' = ReleaseLock(c)
          /\ pc' = [pc EXCEPT ![c] = "done"]
          /\ observedDoc' = [observedDoc EXCEPT ![c] = nodeDoc]
     ELSE IF AcceptConcurrentCreate
          THEN /\ pc' = [pc EXCEPT ![c] = "recheck_after_err"]
               /\ UNCHANGED <<nodeLock, observedDoc>>
          ELSE /\ nodeLock' = ReleaseLock(c)
               /\ pc' = [pc EXCEPT ![c] = "panicked"]
               /\ UNCHANGED observedDoc

\* Commit the transaction: no-op if snapshot already saw a Node, otherwise create_Node.
CommitTxn(c) ==
  /\ pc[c] = "txn_commit"
  /\ IF snap[c] # None
     THEN \* `find_node_doc_id` saw an existing Node inside the txn: no mutation needed.
          /\ nodeLock' = ReleaseLock(c)
          /\ pc' = [pc EXCEPT ![c] = "done"]
          /\ observedDoc' = [observedDoc EXCEPT ![c] = snap[c]]
          /\ UNCHANGED <<nodeDoc, snap, attempts>>
     ELSE IF nodeDoc = None
          THEN \* Node is still absent: commit succeeds, or a real store fault occurs.
               \/ /\ nodeDoc' = c
                  /\ nodeLock' = ReleaseLock(c)
                  /\ pc' = [pc EXCEPT ![c] = "done"]
                  /\ observedDoc' = [observedDoc EXCEPT ![c] = c]
                  /\ UNCHANGED <<snap, attempts>>
               \/ /\ AllowStoreFault
                  /\ IF BlindIgnoreError
                     THEN /\ nodeLock' = ReleaseLock(c)
                          /\ pc' = [pc EXCEPT ![c] = "done"]
                          /\ UNCHANGED observedDoc
                     ELSE IF AcceptConcurrentCreate
                          THEN /\ pc' = [pc EXCEPT ![c] = "recheck_after_err"]
                               /\ UNCHANGED <<nodeLock, observedDoc>>
                          ELSE /\ nodeLock' = ReleaseLock(c)
                               /\ pc' = [pc EXCEPT ![c] = "store_fault"]
                               /\ UNCHANGED observedDoc
                  /\ UNCHANGED <<nodeDoc, snap, attempts>>
          ELSE \* Concurrent writer committed Node since `snap[c]`:
               \* either TransactionConflict (retried while attempts < MaxAttempts)
               \* or unique-index rejection (fails immediately).
               \/ /\ attempts[c] < MaxAttempts
                  /\ pc' = [pc EXCEPT ![c] = "begin_txn"]
                  /\ UNCHANGED <<nodeDoc, nodeLock, snap, attempts, observedDoc>>
               \/ /\ attempts[c] = MaxAttempts
                  /\ ConcurrencyWriteFailed(c)
                  /\ UNCHANGED <<nodeDoc, snap, attempts>>
               \/ \* Unique-index rejection (non-retryable in `transact_write`).
                  /\ ConcurrencyWriteFailed(c)
                  /\ UNCHANGED <<nodeDoc, snap, attempts>>

\* After `transact_write` returns `Err`, recheck `load_node(node, node_did)`.
RecheckAfterErr(c) ==
  /\ pc[c] = "recheck_after_err"
  /\ nodeLock' = ReleaseLock(c)
  /\ IF nodeDoc # None
     THEN /\ pc' = [pc EXCEPT ![c] = "done"]
          /\ observedDoc' = [observedDoc EXCEPT ![c] = nodeDoc]
     ELSE /\ pc' = [pc EXCEPT ![c] = "store_fault"]
          /\ UNCHANGED observedDoc
  /\ UNCHANGED <<nodeDoc, snap, attempts>>

\* An external writer (e.g., `enroll_test_user`) creates the Node outside the mutex.
ExternalCreate ==
  /\ AllowExternalWriter
  /\ nodeDoc = None
  /\ nodeDoc' = External
  /\ UNCHANGED <<nodeLock, pc, snap, attempts, observedDoc>>

Next ==
  \/ \E c \in Callers :
       FastRead(c) \/ AcquireLock(c) \/ Recheck(c) \/ BeginTxn(c)
         \/ CommitTxn(c) \/ RecheckAfterErr(c)
  \/ ExternalCreate

Spec == Init /\ [][Next]_vars

\* No caller panics because of concurrent Node creation.
NoConcurrencyPanic == \A c \in Callers : pc[c] # "panicked"

\* Every caller that completes observes the single committed Node (never masks a store fault).
AllDoneObserveSameNode ==
  \A c \in Callers : pc[c] = "done" => (nodeDoc # None /\ observedDoc[c] = nodeDoc)

\* Two `ensure_test_node` callers never hold concurrent write transactions when serialized.
SingleNodeWrite ==
  \A c1, c2 \in Callers :
    (pc[c1] = "txn_commit" /\ pc[c2] = "txn_commit") => c1 = c2

====
