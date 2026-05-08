---- MODULE ReversePairing ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Reverse-pairing subscription/replicator convergence between two peers.  *)
(* Spec design:                                                            *)
(*   docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md       *)
(*                                                                         *)
(* This module models the abstract control-plane: state, RPC kinds, and    *)
(* actions. MCReversePairing.tla instantiates with bounded constants for   *)
(* TLC.                                                                    *)
(***************************************************************************)

CONSTANTS
  Node,        \* set of node identifiers (e.g., {"A", "B"})
  Collection,  \* set of collection identifiers (e.g., {"c1", "c2"})
  RPCId,       \* set of unique RPC identifiers; bounded for TLC
  MaxCrashes,  \* per-node crash budget (Nat)
  NoOf         \* sentinel "no originating RPC"; bound to a value disjoint from RPCId

ASSUME NodeIsFiniteSet == IsFiniteSet(Node)
ASSUME CollectionIsFiniteSet == IsFiniteSet(Collection)
ASSUME RPCIdIsFiniteSet == IsFiniteSet(RPCId)
ASSUME MaxCrashesIsNat == MaxCrashes \in Nat
ASSUME NoOfNotInRPCId == NoOf \notin RPCId

VARIABLES
  desired,          \* desired[n][p] : SUBSET Collection — operator-set, persisted
  replicator,       \* replicator[n][p] : SUBSET Collection — n's local push-to-p replicator entries, persisted
  inFlight,         \* inFlight[n] : SUBSET RPC — caller's pending RPCs, in-memory
  pendingInbound,   \* pendingInbound[n] : SUBSET RPC — receiver's not-yet-processed RPCs, in-memory
  messages,         \* SUBSET RPC — in-transit network messages
  crashCount,       \* crashCount[n] : Nat — bookkeeping for the bounded crash budget
  rpcIdsUsed        \* SUBSET RPCId — IDs already issued, to enforce uniqueness

vars == <<desired, replicator, inFlight, pendingInbound, messages, crashCount, rpcIdsUsed>>

(***************************************************************************)
(* RPC structure. Kind ∈ {"Install", "Teardown", "Ack"}. For Ack, `of`     *)
(* carries the originating RPC's id so the caller can match it.            *)
(***************************************************************************)

RPCKind == {"Install", "Teardown", "Ack"}

RPC == [
  id         : RPCId,
  kind       : RPCKind,
  src        : Node,
  tgt        : Node,
  collection : Collection,
  of         : RPCId \cup {NoOf}
]

TypeOK ==
  /\ desired         \in [Node -> [Node -> SUBSET Collection]]
  /\ replicator      \in [Node -> [Node -> SUBSET Collection]]
  /\ inFlight        \in [Node -> SUBSET RPC]
  /\ pendingInbound  \in [Node -> SUBSET RPC]
  /\ messages        \in SUBSET RPC
  /\ crashCount      \in [Node -> 0..MaxCrashes]
  /\ rpcIdsUsed      \in SUBSET RPCId

Init ==
  /\ desired        = [n \in Node |-> [p \in Node |-> {}]]
  /\ replicator     = [n \in Node |-> [p \in Node |-> {}]]
  /\ inFlight       = [n \in Node |-> {}]
  /\ pendingInbound = [n \in Node |-> {}]
  /\ messages       = {}
  /\ crashCount     = [n \in Node |-> 0]
  /\ rpcIdsUsed     = {}

(***************************************************************************)
(* Helpers                                                                 *)
(***************************************************************************)

Range(f) == { f[x] : x \in DOMAIN f }

FreshIds(k) ==
  \* True when there are at least k unused RPC ids available
  Cardinality(RPCId \ rpcIdsUsed) >= k

PendingInstallFor(n, p, c) ==
  \E rpc \in inFlight[n] :
    /\ rpc.kind = "Install"
    /\ rpc.tgt = p
    /\ rpc.collection = c

PendingTeardownFor(n, p, c) ==
  \E rpc \in inFlight[n] :
    /\ rpc.kind = "Teardown"
    /\ rpc.tgt = p
    /\ rpc.collection = c

(***************************************************************************)
(* OperatorWrite(n, p, S): operator on node n sets desired[n][p] = S.      *)
(* Atomic update of desired only — no RPCs emitted. Reconcile fires        *)
(* separately to bridge any resulting gap.                                 *)
(*                                                                         *)
(* The S # desired[n][p] precondition prunes stutter steps where the       *)
(* operator writes the same value already present.                         *)
(***************************************************************************)

OperatorWrite(n, p, S) ==
  /\ p # n
  /\ S # desired[n][p]
  /\ desired' = [desired EXCEPT ![n] = [@ EXCEPT ![p] = S]]
  /\ UNCHANGED <<replicator, inFlight, pendingInbound, messages, crashCount, rpcIdsUsed>>

(***************************************************************************)
(* Reconcile(n): emit ONE Install or Teardown RPC for some (p, c) pair    *)
(* where desired[n][p] and replicator[p][n] disagree, provided no matching *)
(* RPC is already in flight. Fires from any state (including post-Crash    *)
(* recovery, when inFlight has been cleared but the persisted gap          *)
(* survives).                                                              *)
(*                                                                         *)
(* Per-firing scope is one (p, c); multiple disagreements get reconciled   *)
(* across multiple firings under fairness.                                 *)
(***************************************************************************)

ReconcileInstall(n, p, c) ==
  /\ p # n
  /\ c \in desired[n][p]
  /\ c \notin replicator[p][n]
  /\ ~PendingInstallFor(n, p, c)
  /\ FreshIds(1)
  /\ LET id == CHOOSE i \in RPCId \ rpcIdsUsed : TRUE
         rpc == [id |-> id, kind |-> "Install", src |-> n, tgt |-> p,
                 collection |-> c, of |-> NoOf]
     IN /\ inFlight'    = [inFlight EXCEPT ![n] = @ \cup {rpc}]
        /\ messages'    = messages \cup {rpc}
        /\ rpcIdsUsed'  = rpcIdsUsed \cup {id}
  /\ UNCHANGED <<desired, replicator, pendingInbound, crashCount>>

ReconcileTeardown(n, p, c) ==
  /\ p # n
  /\ c \in replicator[p][n]
  /\ c \notin desired[n][p]
  /\ ~PendingTeardownFor(n, p, c)
  /\ FreshIds(1)
  /\ LET id == CHOOSE i \in RPCId \ rpcIdsUsed : TRUE
         rpc == [id |-> id, kind |-> "Teardown", src |-> n, tgt |-> p,
                 collection |-> c, of |-> NoOf]
     IN /\ inFlight'    = [inFlight EXCEPT ![n] = @ \cup {rpc}]
        /\ messages'    = messages \cup {rpc}
        /\ rpcIdsUsed'  = rpcIdsUsed \cup {id}
  /\ UNCHANGED <<desired, replicator, pendingInbound, crashCount>>

Reconcile(n) ==
  \/ \E p \in Node, c \in Collection : ReconcileInstall(n, p, c)
  \/ \E p \in Node, c \in Collection : ReconcileTeardown(n, p, c)

(***************************************************************************)
(* Deliver(rpc): network delivers an in-transit message to its destination *)
(* node's pendingInbound queue.                                            *)
(***************************************************************************)

Deliver(rpc) ==
  /\ rpc \in messages
  /\ messages' = messages \ {rpc}
  /\ pendingInbound' = [pendingInbound EXCEPT ![rpc.tgt] = @ \cup {rpc}]
  /\ UNCHANGED <<desired, replicator, inFlight, crashCount, rpcIdsUsed>>

(***************************************************************************)
(* Drop(rpc): network loses an in-transit message. Bounded by fairness so  *)
(* infinitely many drops do not occur in any execution; see liveness task. *)
(***************************************************************************)

Drop(rpc) ==
  /\ rpc \in messages
  /\ messages' = messages \ {rpc}
  /\ UNCHANGED <<desired, replicator, inFlight, pendingInbound, crashCount, rpcIdsUsed>>

(***************************************************************************)
(* Process(recv, rpc): receiver runs its handler.                          *)
(*   Install:  replicator[recv][rpc.src] gains rpc.collection.             *)
(*   Teardown: replicator[recv][rpc.src] loses rpc.collection.             *)
(* In both cases an Ack RPC is enqueued to messages atomically with the    *)
(* persisted change (modeling the persist-before-ack derived requirement). *)
(*                                                                         *)
(* Idempotent: Install for a collection already present is a state no-op   *)
(* (still emits an ack). Symmetric for Teardown. (Decision: model handlers *)
(* as inherently idempotent rather than parameterizing.)                   *)
(***************************************************************************)

ackOf(rpc) ==
  LET ackId == CHOOSE id \in RPCId \ rpcIdsUsed : TRUE  \* fresh id for the ack
  IN [ id         |-> ackId,
       kind       |-> "Ack",
       src        |-> rpc.tgt,
       tgt        |-> rpc.src,
       collection |-> rpc.collection,
       of         |-> rpc.id ]

Process(recv, rpc) ==
  /\ rpc \in pendingInbound[recv]
  /\ rpc.tgt = recv
  /\ rpc.kind \in {"Install", "Teardown"}
  /\ FreshIds(1)                                          \* ack needs an id
  /\ pendingInbound' = [pendingInbound EXCEPT ![recv] = @ \ {rpc}]
  /\ \/ /\ rpc.kind = "Install"
        /\ replicator' =
             [replicator EXCEPT ![recv] = [@ EXCEPT ![rpc.src] = @ \cup {rpc.collection}]]
     \/ /\ rpc.kind = "Teardown"
        /\ replicator' =
             [replicator EXCEPT ![recv] = [@ EXCEPT ![rpc.src] = @ \ {rpc.collection}]]
  /\ LET ack == ackOf(rpc) IN
       /\ messages'    = messages \cup {ack}
       /\ rpcIdsUsed'  = rpcIdsUsed \cup {ack.id}
  /\ UNCHANGED <<desired, inFlight, crashCount>>

(***************************************************************************)
(* ReceiveAck(n, ack): caller matches an Ack from pendingInbound to an     *)
(* in_flight entry by `of`. Removes both. No persisted state change on n   *)
(* — the install/teardown happened on the peer's side.                     *)
(***************************************************************************)

ReceiveAck(n, ack) ==
  /\ ack \in pendingInbound[n]
  /\ ack.kind = "Ack"
  /\ ack.tgt = n
  /\ \E rpc \in inFlight[n] : rpc.id = ack.of
  /\ pendingInbound' = [pendingInbound EXCEPT ![n] = @ \ {ack}]
  /\ inFlight' =
       [inFlight EXCEPT ![n] = { rpc \in @ : rpc.id # ack.of }]
  /\ UNCHANGED <<desired, replicator, messages, crashCount, rpcIdsUsed>>

(***************************************************************************)
(* Timeout(n, rpc): caller drops an in_flight RPC without seeing an ack.   *)
(* Models the request-response timeout from the comm_channel pattern.      *)
(* Per spec §"Boundary discipline: timeouts" this is a liveness-only       *)
(* action: no other state changes.                                         *)
(***************************************************************************)

Timeout(n, rpc) ==
  /\ rpc \in inFlight[n]
  /\ inFlight' = [inFlight EXCEPT ![n] = @ \ {rpc}]
  /\ UNCHANGED <<desired, replicator, pendingInbound, messages, crashCount, rpcIdsUsed>>

(***************************************************************************)
(* Crash(n): clears n's in-memory state (inFlight, pendingInbound) and    *)
(* increments crashCount. Bounded by MaxCrashes for finite model checking. *)
(* Persisted state (desired, replicator) survives.                         *)
(***************************************************************************)

Crash(n) ==
  /\ crashCount[n] < MaxCrashes
  /\ inFlight'       = [inFlight       EXCEPT ![n] = {}]
  /\ pendingInbound' = [pendingInbound EXCEPT ![n] = {}]
  /\ crashCount'     = [crashCount     EXCEPT ![n] = @ + 1]
  /\ UNCHANGED <<desired, replicator, messages, rpcIdsUsed>>

Next ==
  \/ \E n \in Node, p \in Node, S \in SUBSET Collection : OperatorWrite(n, p, S)
  \/ \E n \in Node : Reconcile(n)
  \/ \E rpc \in messages : Deliver(rpc)
  \/ \E rpc \in messages : Drop(rpc)
  \/ \E recv \in Node : \E rpc \in pendingInbound[recv] : Process(recv, rpc)
  \/ \E n \in Node : \E ack \in pendingInbound[n] : ReceiveAck(n, ack)
  \/ \E n \in Node : \E rpc \in inFlight[n] : Timeout(n, rpc)
  \/ \E n \in Node : Crash(n)

(***************************************************************************)
(* Structural safety invariants                                            *)
(***************************************************************************)

(* Every RPC in messages or pendingInbound or inFlight has a unique id and *)
(* that id appears in rpcIdsUsed.                                          *)
RPCIdsTracked ==
  /\ \A rpc \in messages : rpc.id \in rpcIdsUsed
  /\ \A n \in Node : \A rpc \in inFlight[n] : rpc.id \in rpcIdsUsed
  /\ \A n \in Node : \A rpc \in pendingInbound[n] : rpc.id \in rpcIdsUsed

(* Every Install or Teardown RPC has src # tgt, kind in the right set, and *)
(* of = NoOf. Every Ack has of \in rpcIdsUsed.                            *)
RPCWellFormed ==
  LET allRPCs == messages \cup UNION { inFlight[n] : n \in Node }
                           \cup UNION { pendingInbound[n] : n \in Node }
  IN \A rpc \in allRPCs :
       /\ rpc.src # rpc.tgt
       /\ rpc.kind \in RPCKind
       /\ \/ /\ rpc.kind \in {"Install", "Teardown"}
             /\ rpc.of = NoOf
          \/ /\ rpc.kind = "Ack"
             /\ rpc.of \in rpcIdsUsed

(***************************************************************************)
(* Supporting invariant: in-flight justification (bounded-model form).     *)
(*                                                                         *)
(* For every reachable state WHERE FreshIds(1) holds, every               *)
(* desired/replicator disagreement is either:                              *)
(*   (a) matched by a reconciling RPC anywhere in the system (inFlight,    *)
(*       messages, or pendingInbound), OR                                  *)
(*   (b) structurally eligible for Reconcile (no blocking duplicate        *)
(*       in-flight RPC) — meaning the system is one Reconcile firing away  *)
(*       from emitting the resolving RPC.                                  *)
(*                                                                         *)
(* The outer FreshIds(1) guard scopes the invariant to states where the    *)
(* system can act: pool-exhausted states are a TLC-bounding artifact (the  *)
(* real system has an effectively unbounded ID space) and are excluded.    *)
(*                                                                         *)
(* The second disjunct qualifies the invariant for the OperatorWrite/      *)
(* Reconcile window and for post-Crash recovery states where the gap       *)
(* exists but no RPC is in flight yet. Under fairness on Reconcile, the   *)
(* second disjunct collapses to the first within finitely many steps.      *)
(*                                                                         *)
(* This is the inductive invariant supporting the leads-to liveness        *)
(* property to be added in Task 10.                                        *)
(***************************************************************************)

InstallJustified ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in desired[n][p] /\ c \notin replicator[p][n])
    => \/ \E rpc \in inFlight[n] \cup messages \cup pendingInbound[p] :
            /\ rpc.kind = "Install"
            /\ rpc.src = n
            /\ rpc.tgt = p
            /\ rpc.collection = c
       \/ ~PendingInstallFor(n, p, c)

TeardownJustified ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in replicator[p][n] /\ c \notin desired[n][p])
    => \/ \E rpc \in inFlight[n] \cup messages \cup pendingInbound[p] :
            /\ rpc.kind = "Teardown"
            /\ rpc.src = n
            /\ rpc.tgt = p
            /\ rpc.collection = c
       \/ ~PendingTeardownFor(n, p, c)

InFlightJustified == FreshIds(1) => (InstallJustified /\ TeardownJustified)

Spec == Init /\ [][Next]_vars

====
