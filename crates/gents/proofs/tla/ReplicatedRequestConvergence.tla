---- MODULE ReplicatedRequestConvergence ----
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
  Owner,
  ReplicaHolder,
  DeltaId,
  MaxDrops,
  MaxCrashes,
  TerminalKind,
  Cap,
  ReplayOnRecovery,
  AllowPeerClaim

Node == {Owner} \cup ReplicaHolder

NonTerminal == {"Pending", "Claimed", "Processing"}
LifecycleState == NonTerminal \cup TerminalKind

ASSUME OwnerNotAReplica == Owner \notin ReplicaHolder
ASSUME ReplicaHolderIsFiniteSet == IsFiniteSet(ReplicaHolder)
ASSUME ReplicaHolderNonEmpty == ReplicaHolder # {}
ASSUME DeltaIdIsFiniteSet == IsFiniteSet(DeltaId)
ASSUME MaxDropsIsNat == MaxDrops \in Nat
ASSUME MaxCrashesIsNat == MaxCrashes \in Nat
ASSUME TerminalKindNonEmpty == TerminalKind # {}
ASSUME TerminalKindDisjoint == TerminalKind \cap NonTerminal = {}
ASSUME CapIsPositiveNat == Cap \in (Nat \ {0})
ASSUME ReplayOnRecoveryIsBoolean == ReplayOnRecovery \in BOOLEAN
ASSUME AllowPeerClaimIsBoolean == AllowPeerClaim \in BOOLEAN

Delta == [
  id     : DeltaId,
  target : ReplicaHolder,
  value  : TerminalKind
]

VARIABLES
  reqState,
  messages,
  pendingInbound,
  deltaIdsUsed,
  dropCount,
  crashCount,
  emitCount,
  peerOnline,
  replayPending,
  replayCount,
  acked

vars == <<
  reqState,
  messages,
  pendingInbound,
  deltaIdsUsed,
  dropCount,
  crashCount,
  emitCount,
  peerOnline,
  replayPending,
  replayCount,
  acked
>>

IsTerminal(s) == s \in TerminalKind

AllDeltas == messages \cup pendingInbound

TypeOK ==
  /\ reqState        \in [Node -> LifecycleState]
  /\ messages        \in SUBSET Delta
  /\ pendingInbound  \in SUBSET Delta
  /\ deltaIdsUsed    \in SUBSET DeltaId
  /\ dropCount       \in 0..MaxDrops
  /\ crashCount      \in 0..MaxCrashes
  /\ emitCount       \in 0..Cap
  /\ peerOnline      \in [ReplicaHolder -> BOOLEAN]
  /\ replayPending   \in [ReplicaHolder -> BOOLEAN]
  /\ replayCount     \in [ReplicaHolder -> 0..1]
  /\ acked           \in [ReplicaHolder -> BOOLEAN]

Init ==
  /\ reqState        = [n \in Node |-> "Pending"]
  /\ messages        = {}
  /\ pendingInbound  = {}
  /\ deltaIdsUsed    = {}
  /\ dropCount       = 0
  /\ crashCount      = 0
  /\ emitCount       = 0
  /\ peerOnline      \in [ReplicaHolder -> BOOLEAN]
  /\ replayPending   = [peer \in ReplicaHolder |-> FALSE]
  /\ replayCount     = [peer \in ReplicaHolder |-> 0]
  /\ acked           = [peer \in ReplicaHolder |-> FALSE]

Claim ==
  /\ reqState[Owner] = "Pending"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

Process ==
  /\ reqState[Owner] = "Claimed"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Processing"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

Terminalize(k) ==
  /\ k \in TerminalKind
  /\ reqState[Owner] = "Processing"
  /\ reqState' = [reqState EXCEPT ![Owner] = k]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

FreshDeltaIds(k) == Cardinality(DeltaId \ deltaIdsUsed) >= k

OnlinePeers == {peer \in ReplicaHolder : peerOnline[peer]}

\* DefraDB reports whether a replicator acknowledged the owner's current
\* heads, so the owner re-drives only while some party peer has not. Each
\* re-drive authors a new head that every peer must acknowledge again.
EmitTerminalDelta ==
  /\ IsTerminal(reqState[Owner])
  /\ emitCount < Cap
  /\ \E peer \in ReplicaHolder : ~acked[peer]
  /\ AllDeltas = {}
  /\ FreshDeltaIds(1)
  /\ LET id == CHOOSE i \in DeltaId \ deltaIdsUsed : TRUE
         wave == {[id |-> id, target |-> peer, value |-> reqState[Owner]]
                    : peer \in OnlinePeers}
     IN /\ messages' = messages \cup wave
        /\ deltaIdsUsed' = deltaIdsUsed \cup {id}
        /\ emitCount'    = emitCount + 1
  /\ acked' = [peer \in ReplicaHolder |-> FALSE]
  /\ UNCHANGED <<reqState, pendingInbound, dropCount, crashCount,
                  peerOnline, replayPending, replayCount>>

DeliverDelta(d) ==
  /\ d \in messages
  /\ peerOnline[d.target]
  /\ messages'       = messages \ {d}
  /\ pendingInbound' = pendingInbound \cup {d}
  /\ UNCHANGED <<reqState, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

DropDelta(d) ==
  /\ d \in messages
  /\ dropCount < MaxDrops
  /\ messages'  = messages \ {d}
  /\ dropCount' = dropCount + 1
  /\ UNCHANGED <<reqState, pendingInbound, deltaIdsUsed, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

PersistDeltaOnPeer(d) ==
  /\ d \in pendingInbound
  /\ pendingInbound' = pendingInbound \ {d}
  /\ reqState'       = [reqState EXCEPT ![d.target] = d.value]
  /\ acked'          = [acked EXCEPT ![d.target] = TRUE]
  /\ UNCHANGED <<messages, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

RecoverPeer(peer) ==
  /\ ReplayOnRecovery
  /\ peer \in ReplicaHolder
  /\ ~peerOnline[peer]
  /\ peerOnline'    = [peerOnline EXCEPT ![peer] = TRUE]
  /\ replayPending' = [replayPending EXCEPT ![peer] = TRUE]
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount,
                  crashCount, emitCount, replayCount, acked>>

ReplayTerminalSnapshot(peer) ==
  /\ ReplayOnRecovery
  /\ peer \in ReplicaHolder
  /\ peerOnline[peer]
  /\ replayPending[peer]
  /\ ~acked[peer]
  /\ IsTerminal(reqState[Owner])
  /\ reqState'      = [reqState EXCEPT ![peer] = reqState[Owner]]
  /\ replayPending' = [replayPending EXCEPT ![peer] = FALSE]
  /\ replayCount'   = [replayCount EXCEPT ![peer] = 1]
  /\ acked'         = [acked EXCEPT ![peer] = TRUE]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline>>

\* A reinstall would discard DefraDB's retry markers and acknowledgement
\* records while delivering nothing the peer lacks.
SkipAcknowledgedReplay(peer) ==
  /\ ReplayOnRecovery
  /\ peer \in ReplicaHolder
  /\ replayPending[peer]
  /\ acked[peer]
  /\ replayPending' = [replayPending EXCEPT ![peer] = FALSE]
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount,
                  crashCount, emitCount, peerOnline, replayCount, acked>>

CrashPeer(peer) ==
  /\ peer \in ReplicaHolder
  /\ crashCount < MaxCrashes
  /\ pendingInbound' = {d \in pendingInbound : d.target # peer}
  /\ crashCount'     = crashCount + 1
  /\ UNCHANGED <<reqState, messages, deltaIdsUsed, dropCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

CrashOwner ==
  /\ crashCount < MaxCrashes
  /\ crashCount' = crashCount + 1
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

PeerClaimsForeign(peer) ==
  /\ AllowPeerClaim
  /\ peer \in ReplicaHolder
  /\ reqState[peer] = "Pending"
  /\ reqState' = [reqState EXCEPT ![peer] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked>>

SingleClaimer ==
  \A n \in ReplicaHolder :
    /\ reqState[n] \notin {"Claimed", "Processing"}
    /\ (IsTerminal(reqState[n]) =>
          /\ IsTerminal(reqState[Owner])
          /\ reqState[n] = reqState[Owner])

DeltaBackedByOwnerTerminal ==
  \A d \in AllDeltas :
    /\ IsTerminal(reqState[Owner])
    /\ d.value = reqState[Owner]

DeltaIdsTracked ==
  \A d \in AllDeltas : d.id \in deltaIdsUsed

StateBound ==
  Cardinality(deltaIdsUsed) <= Cap

AckedImpliesConverged ==
  \A peer \in ReplicaHolder :
    acked[peer] => /\ IsTerminal(reqState[Owner])
                   /\ reqState[peer] = reqState[Owner]

ReplayBound ==
  \A peer \in ReplicaHolder : replayCount[peer] <= 1

Next ==
  \/ Claim
  \/ Process
  \/ \E k \in TerminalKind : Terminalize(k)
  \/ EmitTerminalDelta
  \/ \E d \in messages : DeliverDelta(d)
  \/ \E d \in messages : DropDelta(d)
  \/ \E d \in pendingInbound : PersistDeltaOnPeer(d)
  \/ \E peer \in ReplicaHolder : RecoverPeer(peer)
  \/ \E peer \in ReplicaHolder : ReplayTerminalSnapshot(peer)
  \/ \E peer \in ReplicaHolder : SkipAcknowledgedReplay(peer)
  \/ \E peer \in ReplicaHolder : PeerClaimsForeign(peer)
  \/ \E peer \in ReplicaHolder : CrashPeer(peer)
  \/ CrashOwner

Fairness ==
  /\ WF_vars(EmitTerminalDelta)
  /\ \A peer \in ReplicaHolder : WF_vars(RecoverPeer(peer))
  /\ \A peer \in ReplicaHolder : WF_vars(ReplayTerminalSnapshot(peer))
  /\ \A peer \in ReplicaHolder : WF_vars(SkipAcknowledgedReplay(peer))
  /\ WF_vars(\E d \in messages : DeliverDelta(d))
  /\ WF_vars(\E d \in pendingInbound : PersistDeltaOnPeer(d))

Spec == Init /\ [][Next]_vars /\ Fairness

TerminalConverges ==
  \A peer \in ReplicaHolder :
    IsTerminal(reqState[Owner]) ~> (reqState[peer] = reqState[Owner])

====
