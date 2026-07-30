---- MODULE ReversePairing ----
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
  Node,
  Collection,
  RPCId,
  MaxCrashes,
  NoOf

ASSUME NodeIsFiniteSet == IsFiniteSet(Node)
ASSUME CollectionIsFiniteSet == IsFiniteSet(Collection)
ASSUME RPCIdIsFiniteSet == IsFiniteSet(RPCId)
ASSUME MaxCrashesIsNat == MaxCrashes \in Nat
ASSUME NoOfNotInRPCId == NoOf \notin RPCId

VARIABLES
  desired,
  replicator,
  inFlight,
  pendingInbound,
  messages,
  crashCount,
  rpcIdsUsed

vars == <<desired, replicator, inFlight, pendingInbound, messages, crashCount, rpcIdsUsed>>

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

FreshIds(k) ==
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

OperatorWrite(n, p, S) ==
  /\ p # n
  /\ S # desired[n][p]
  /\ desired' = [desired EXCEPT ![n] = [@ EXCEPT ![p] = S]]
  /\ UNCHANGED <<replicator, inFlight, pendingInbound, messages, crashCount, rpcIdsUsed>>

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

Deliver(rpc) ==
  /\ rpc \in messages
  /\ messages' = messages \ {rpc}
  /\ pendingInbound' = [pendingInbound EXCEPT ![rpc.tgt] = @ \cup {rpc}]
  /\ UNCHANGED <<desired, replicator, inFlight, crashCount, rpcIdsUsed>>

Drop(rpc) ==
  /\ rpc \in messages
  /\ messages' = messages \ {rpc}
  /\ UNCHANGED <<desired, replicator, inFlight, pendingInbound, crashCount, rpcIdsUsed>>

ackOf(rpc) ==
  LET ackId == CHOOSE id \in RPCId \ rpcIdsUsed : TRUE
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
  /\ FreshIds(1)
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

ReceiveAck(n, ack) ==
  /\ ack \in pendingInbound[n]
  /\ ack.kind = "Ack"
  /\ ack.tgt = n
  /\ \E rpc \in inFlight[n] : rpc.id = ack.of
  /\ pendingInbound' = [pendingInbound EXCEPT ![n] = @ \ {ack}]
  /\ inFlight' =
       [inFlight EXCEPT ![n] = { rpc \in @ : rpc.id # ack.of }]
  /\ UNCHANGED <<desired, replicator, messages, crashCount, rpcIdsUsed>>

Timeout(n, rpc) ==
  /\ rpc \in inFlight[n]
  /\ inFlight' = [inFlight EXCEPT ![n] = @ \ {rpc}]
  /\ UNCHANGED <<desired, replicator, pendingInbound, messages, crashCount, rpcIdsUsed>>

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

RPCIdsTracked ==
  /\ \A rpc \in messages : rpc.id \in rpcIdsUsed
  /\ \A n \in Node : \A rpc \in inFlight[n] : rpc.id \in rpcIdsUsed
  /\ \A n \in Node : \A rpc \in pendingInbound[n] : rpc.id \in rpcIdsUsed

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

Fairness ==
  /\ WF_vars(\E rpc \in messages : Deliver(rpc))
  /\ WF_vars(\E recv \in Node : \E rpc \in pendingInbound[recv] : Process(recv, rpc))
  /\ WF_vars(\E n \in Node : \E ack \in pendingInbound[n] : ReceiveAck(n, ack))
  /\ WF_vars(\E n \in Node : \E rpc \in inFlight[n] : Timeout(n, rpc))
  /\ \A n \in Node : SF_vars(Reconcile(n))

Spec == Init /\ [][Next]_vars /\ Fairness

InstallConverges ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in desired[n][p])
      ~> (c \in replicator[p][n] \/ c \notin desired[n][p])

TeardownConverges ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \notin desired[n][p] /\ c \in replicator[p][n])
      ~> (c \notin replicator[p][n] \/ c \in desired[n][p])

Convergence == InstallConverges /\ TeardownConverges

StateBound == Cardinality(rpcIdsUsed) <= 4

====
