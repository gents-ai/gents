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
(* Supporting invariant: in-flight justification.                          *)
(*                                                                         *)
(* For every reachable state, every desired/replicator disagreement is     *)
(* either:                                                                 *)
(*   (a) matched by a reconciling RPC anywhere in the system (inFlight,   *)
(*       messages, or pendingInbound on the peer side), OR                *)
(*   (b) eligible for Reconcile to fire — i.e. ReconcileInstall(n,p,c)   *)
(*       (resp. ReconcileTeardown) is enabled. The four conjuncts in the *)
(*       second disjunct are exactly the missing ReconcileInstall          *)
(*       preconditions beyond the antecedent (n # p, disagreement):       *)
(*       no blocking duplicate in-flight RPC, and FreshIds(1).             *)
(*                                                                         *)
(* Reading: the system is never simultaneously (i) in disagreement, (ii)  *)
(* without a covering RPC anywhere, AND (iii) unable to emit one because  *)
(* the RPC pool is exhausted. The third condition is the bounded-model    *)
(* artifact noted below.                                                   *)
(*                                                                         *)
(* Bounded-model caveat: TLC may report a violation of this invariant in  *)
(* pool-exhausted states where every RPCId has been issued and the        *)
(* covering RPC has been Drop()ped. This is an artifact of the finite     *)
(* RPCId set; the real system has an effectively unbounded id space and   *)
(* the violation cannot occur. Mitigation: bump `RPCId` in                *)
(* `MCReversePairing.cfg` until the artifact is unreachable, or accept    *)
(* the artifact and document it.                                           *)
(*                                                                         *)
(* This is the inductive invariant supporting the leads-to liveness        *)
(* property in Task 10.                                                    *)
(***************************************************************************)

InstallJustified ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in desired[n][p] /\ c \notin replicator[p][n])
    => \/ \E rpc \in inFlight[n] \cup messages \cup pendingInbound[p] :
            /\ rpc.kind = "Install"
            /\ rpc.src = n
            /\ rpc.tgt = p
            /\ rpc.collection = c
       \/ /\ ~PendingInstallFor(n, p, c)
          /\ FreshIds(1)

TeardownJustified ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in replicator[p][n] /\ c \notin desired[n][p])
    => \/ \E rpc \in inFlight[n] \cup messages \cup pendingInbound[p] :
            /\ rpc.kind = "Teardown"
            /\ rpc.src = n
            /\ rpc.tgt = p
            /\ rpc.collection = c
       \/ /\ ~PendingTeardownFor(n, p, c)
          /\ FreshIds(1)

InFlightJustified == InstallJustified /\ TeardownJustified

(***************************************************************************)
(* Fairness.                                                               *)
(*                                                                         *)
(* Weak fairness on Deliver, Process, and ReceiveAck — once an RPC is      *)
(* in messages or pendingInbound, the action that consumes it stays        *)
(* continuously enabled (Drop and Timeout race with these but cannot       *)
(* indefinitely block them from firing).                                   *)
(*                                                                         *)
(* Strong fairness on per-node Reconcile. SF (rather than WF) is essential *)
(* because OperatorWrite can transiently disable a Reconcile precondition  *)
(* (e.g. operator flips desired between {} and {c1}); WF would only        *)
(* require firing if the action stays continuously enabled. SF requires    *)
(* firing whenever the action is enabled infinitely often, which matches  *)
(* the operational behavior — every pass through a disagreement-enabled    *)
(* state gets a chance to fire.                                            *)
(*                                                                         *)
(* Process and ReceiveAck use nested existentials because the second       *)
(* binding (rpc \in pendingInbound[recv]) depends on the first variable    *)
(* (recv); TLA+ does not allow that with comma-separated bounds inside     *)
(* WF_vars(\E ... : ...).                                                  *)
(*                                                                         *)
(* Reconcile is per-node (\A outside the fairness operator) so each node   *)
(* makes progress independently — important for post-Crash recovery.       *)
(***************************************************************************)

Fairness ==
  /\ WF_vars(\E rpc \in messages : Deliver(rpc))
  /\ WF_vars(\E recv \in Node : \E rpc \in pendingInbound[recv] : Process(recv, rpc))
  /\ WF_vars(\E n \in Node : \E ack \in pendingInbound[n] : ReceiveAck(n, ack))
  /\ WF_vars(\E n \in Node : \E rpc \in inFlight[n] : Timeout(n, rpc))
  /\ \A n \in Node : SF_vars(Reconcile(n))

Spec == Init /\ [][Next]_vars /\ Fairness

(***************************************************************************)
(* Liveness: leads-to convergence.                                         *)
(*                                                                         *)
(* Any desired/replicator disagreement is eventually resolved — either by *)
(* the disagreement being closed (replicator catches up) or by the         *)
(* operator retracting/re-issuing the desired entry (P itself ceases to   *)
(* hold).                                                                  *)
(*                                                                         *)
(* The disjunctive form `Q \/ ~P` is necessary because OperatorWrite is   *)
(* unconstrained — the operator may flip desired arbitrarily, and a leads- *)
(* to of the form `P ~> Q` would be falsified by traces where P holds      *)
(* transiently before the operator retracts it (and Reconcile never gets  *)
(* a chance to complete). The system is converging on whatever desired    *)
(* eventually settles to; the property here expresses progress at each    *)
(* moment of observed disagreement, not stability of intermediate values. *)
(***************************************************************************)

InstallConverges ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in desired[n][p])
      ~> (c \in replicator[p][n] \/ c \notin desired[n][p])

TeardownConverges ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \notin desired[n][p] /\ c \in replicator[p][n])
      ~> (c \notin replicator[p][n] \/ c \in desired[n][p])

Convergence == InstallConverges /\ TeardownConverges

(***************************************************************************)
(* State constraint for bounded liveness checking. The leads-to property   *)
(* requires the RPCId pool to have headroom whenever a covering RPC needs *)
(* to be issued; if the pool exhausts before convergence completes, the   *)
(* model gets stuck (a bounded-model artifact, not a real-system bug).    *)
(* StateBound truncates exploration well below pool exhaustion, leaving    *)
(* headroom for Reconcile to fire.                                         *)
(***************************************************************************)

StateBound == Cardinality(rpcIdsUsed) <= 4

====
