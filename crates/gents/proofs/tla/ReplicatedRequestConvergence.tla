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
  AllowPeerClaim,
  Desktop,
  ModelDelivery,
  UntypedRead,
  HealthRereads,
  MaxPairingDrops

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
ASSUME DesktopIsARequestParty == Desktop \in ReplicaHolder
ASSUME ModelDeliveryIsBoolean == ModelDelivery \in BOOLEAN
ASSUME UntypedReadIsBoolean == UntypedRead \in BOOLEAN
ASSUME HealthRereadsIsBoolean == HealthRereads \in BOOLEAN
ASSUME MaxPairingDropsIsNat == MaxPairingDrops \in Nat

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
  acked,
  \* The desktop's outbound leg, the return leg of the canonical AgentSession
  \* row, and the desktop's selected-session projection (ModelDelivery).
  ownerHolds,
  pairingUp,
  pairingDrops,
  desktopWrote,
  claimSeen,
  rowSeen,
  desktopView,
  eventPending

deliveryVars == <<
  ownerHolds,
  pairingUp,
  pairingDrops,
  desktopWrote,
  claimSeen,
  rowSeen,
  desktopView,
  eventPending
>>

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
  acked,
  deliveryVars
>>

IsTerminal(s) == s \in TerminalKind

AllDeltas == messages \cup pendingInbound

\* The owner creates the canonical AgentSession row when it claims.
OwnerClaimed == reqState[Owner] # "Pending"

\* What the desktop's selected-session read may show: the four delivery
\* states, `NoRequest` before anything was sent, and `Error`, the old
\* untyped read failure.
DesktopViews == {"NoRequest", "Delivering", "NotConnected", "Accepted", "Visible", "Error"}

\* `deriveSessionDelivery` in Proofs/Client/Types.lean over the desktop's own
\* copies of the four signals: its AgentRequest write, the pairing, the
\* node's claim or later lifecycle as replicated back, and the row.
Classify ==
  IF rowSeen THEN "Visible"
  ELSE IF ~desktopWrote THEN "NoRequest"
  ELSE IF claimSeen \/ IsTerminal(reqState[Desktop]) THEN "Accepted"
  ELSE IF pairingUp THEN "Delivering"
  ELSE "NotConnected"

\* The pre-fix read failed whenever the row was absent.
ReadResult == IF UntypedRead /\ ~rowSeen THEN "Error" ELSE Classify

\* A store or pairing-health event the desktop has not read yet.
StoreEvent == ModelDelivery
PairingEvent == ModelDelivery /\ HealthRereads

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
  /\ ownerHolds      \in BOOLEAN
  /\ pairingUp       \in BOOLEAN
  /\ pairingDrops    \in 0..MaxPairingDrops
  /\ desktopWrote    \in BOOLEAN
  /\ claimSeen       \in BOOLEAN
  /\ rowSeen         \in BOOLEAN
  /\ desktopView     \in DesktopViews
  /\ eventPending    \in BOOLEAN

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
  /\ ownerHolds      = ~ModelDelivery
  /\ pairingUp       \in IF ModelDelivery THEN BOOLEAN ELSE {TRUE}
  /\ pairingDrops    = 0
  /\ desktopWrote    = FALSE
  /\ claimSeen       = FALSE
  /\ rowSeen         = FALSE
  /\ desktopView     = "NoRequest"
  /\ eventPending    = FALSE

\* Claiming also creates the canonical AgentSession row (`OwnerClaimed`).
Claim ==
  /\ ownerHolds
  /\ reqState[Owner] = "Pending"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked, deliveryVars>>

Process ==
  /\ reqState[Owner] = "Claimed"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Processing"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked,
                  deliveryVars>>

Terminalize(k) ==
  /\ k \in TerminalKind
  /\ reqState[Owner] = "Processing"
  /\ reqState' = [reqState EXCEPT ![Owner] = k]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked,
                  deliveryVars>>

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
                  peerOnline, replayPending, replayCount,
                  deliveryVars>>

DeliverDelta(d) ==
  /\ d \in messages
  /\ peerOnline[d.target]
  /\ messages'       = messages \ {d}
  /\ pendingInbound' = pendingInbound \cup {d}
  /\ UNCHANGED <<reqState, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked,
                  deliveryVars>>

DropDelta(d) ==
  /\ d \in messages
  /\ dropCount < MaxDrops
  /\ messages'  = messages \ {d}
  /\ dropCount' = dropCount + 1
  /\ UNCHANGED <<reqState, pendingInbound, deltaIdsUsed, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked,
                  deliveryVars>>

PersistDeltaOnPeer(d) ==
  /\ d \in pendingInbound
  /\ pendingInbound' = pendingInbound \ {d}
  /\ reqState'       = [reqState EXCEPT ![d.target] = d.value]
  /\ acked'          = [acked EXCEPT ![d.target] = TRUE]
  /\ eventPending'   = (eventPending \/ (StoreEvent /\ d.target = Desktop))
  /\ UNCHANGED <<messages, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, ownerHolds, pairingUp,
                  pairingDrops, desktopWrote, claimSeen, rowSeen, desktopView>>

RecoverPeer(peer) ==
  /\ ReplayOnRecovery
  /\ peer \in ReplicaHolder
  /\ ~peerOnline[peer]
  /\ peerOnline'    = [peerOnline EXCEPT ![peer] = TRUE]
  /\ replayPending' = [replayPending EXCEPT ![peer] = TRUE]
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount,
                  crashCount, emitCount, replayCount, acked,
                  deliveryVars>>

\* DefraDB owns recovery of installed replication work. Gents ensures the same
\* authorized configuration without deleting native retry or acknowledgement state.
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
  /\ eventPending'  = (eventPending \/ (StoreEvent /\ peer = Desktop))
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, ownerHolds, pairingUp, pairingDrops,
                  desktopWrote, claimSeen, rowSeen, desktopView>>

\* The native ensure operation preserves acknowledgements and retry markers;
\* an already acknowledged peer needs no additional delivery.
SkipAcknowledgedReplay(peer) ==
  /\ ReplayOnRecovery
  /\ peer \in ReplicaHolder
  /\ replayPending[peer]
  /\ acked[peer]
  /\ replayPending' = [replayPending EXCEPT ![peer] = FALSE]
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount,
                  crashCount, emitCount, peerOnline, replayCount, acked,
                  deliveryVars>>

CrashPeer(peer) ==
  /\ peer \in ReplicaHolder
  /\ crashCount < MaxCrashes
  /\ pendingInbound' = {d \in pendingInbound : d.target # peer}
  /\ crashCount'     = crashCount + 1
  /\ UNCHANGED <<reqState, messages, deltaIdsUsed, dropCount, emitCount,
                  peerOnline, replayPending, replayCount, acked,
                  deliveryVars>>

CrashOwner ==
  /\ crashCount < MaxCrashes
  /\ crashCount' = crashCount + 1
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, emitCount,
                  peerOnline, replayPending, replayCount, acked,
                  deliveryVars>>

PeerClaimsForeign(peer) ==
  /\ AllowPeerClaim
  /\ peer \in ReplicaHolder
  /\ reqState[peer] = "Pending"
  /\ reqState' = [reqState EXCEPT ![peer] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount, acked,
                  deliveryVars>>

----
\* The desktop as request party (ModelDelivery). It writes its AgentRequest
\* locally, which raises a store event, and the client-to-runtime pairing
\* carries it to the owner. The pairing goes down at most MaxPairingDrops
\* times; each change raises a pairing-health event, which re-reads the
\* waiting session unless HealthRereads is off. The owner's claim and the
\* canonical row return over the party replicator; each arrival is a store
\* event. Events alone drive reads: nothing reads on a timer.

DesktopWrite ==
  /\ ModelDelivery
  /\ ~desktopWrote
  /\ desktopWrote' = TRUE
  /\ eventPending' = TRUE
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, replayPending, replayCount, acked,
                  ownerHolds, pairingUp, pairingDrops, claimSeen, rowSeen, desktopView>>

DeliverRequest ==
  /\ ModelDelivery
  /\ desktopWrote
  /\ pairingUp
  /\ ~ownerHolds
  /\ ownerHolds' = TRUE
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, replayPending, replayCount, acked,
                  pairingUp, pairingDrops, desktopWrote, claimSeen, rowSeen, desktopView,
                  eventPending>>

PairingDrop ==
  /\ ModelDelivery
  /\ pairingUp
  /\ pairingDrops < MaxPairingDrops
  /\ pairingUp'    = FALSE
  /\ pairingDrops' = pairingDrops + 1
  /\ eventPending' = (eventPending \/ PairingEvent)
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, replayPending, replayCount, acked,
                  ownerHolds, desktopWrote, claimSeen, rowSeen, desktopView>>

PairingRestore ==
  /\ ModelDelivery
  /\ ~pairingUp
  /\ pairingUp'    = TRUE
  /\ eventPending' = (eventPending \/ PairingEvent)
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, replayPending, replayCount, acked,
                  ownerHolds, pairingDrops, desktopWrote, claimSeen, rowSeen, desktopView>>

ReturnClaim ==
  /\ ModelDelivery
  /\ OwnerClaimed
  /\ peerOnline[Desktop]
  /\ ~claimSeen
  /\ claimSeen'    = TRUE
  /\ eventPending' = TRUE
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, replayPending, replayCount, acked,
                  ownerHolds, pairingUp, pairingDrops, desktopWrote, rowSeen, desktopView>>

ReturnRow ==
  /\ ModelDelivery
  /\ OwnerClaimed
  /\ peerOnline[Desktop]
  /\ ~rowSeen
  /\ rowSeen'      = TRUE
  /\ eventPending' = TRUE
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, replayPending, replayCount, acked,
                  ownerHolds, pairingUp, pairingDrops, desktopWrote, claimSeen, desktopView>>

\* One read consumes every event raised before it.
DesktopRead ==
  /\ ModelDelivery
  /\ eventPending
  /\ desktopView'  = ReadResult
  /\ eventPending' = FALSE
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline, replayPending, replayCount, acked,
                  ownerHolds, pairingUp, pairingDrops, desktopWrote, claimSeen, rowSeen>>

----
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

\* No stale state: once the desktop has read every event, its projection is
\* the classification of the signals it now holds.
ProjectionCurrent ==
  ~eventPending => desktopView = Classify

\* A request the desktop wrote can still become visible until its row
\* arrives, so the read never shows a terminal error meanwhile.
NoTerminalErrorWhileDeliverable ==
  (desktopWrote /\ ~rowSeen) => desktopView # "Error"

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
  \/ DesktopWrite
  \/ DeliverRequest
  \/ PairingDrop
  \/ PairingRestore
  \/ ReturnClaim
  \/ ReturnRow
  \/ DesktopRead

Fairness ==
  /\ WF_vars(EmitTerminalDelta)
  /\ \A peer \in ReplicaHolder : WF_vars(RecoverPeer(peer))
  /\ \A peer \in ReplicaHolder : WF_vars(ReplayTerminalSnapshot(peer))
  /\ \A peer \in ReplicaHolder : WF_vars(SkipAcknowledgedReplay(peer))
  /\ WF_vars(\E d \in messages : DeliverDelta(d))
  /\ WF_vars(\E d \in pendingInbound : PersistDeltaOnPeer(d))

Spec == Init /\ [][Next]_vars /\ Fairness

\* Delivery liveness assumes the pairing is eventually up for good (bounded
\* drops and fair restore), the owner eventually claims what it holds, the
\* replicators eventually carry each leg, and every event is eventually read.
DeliverySpec ==
  /\ Spec
  /\ WF_vars(PairingRestore)
  /\ WF_vars(DeliverRequest)
  /\ WF_vars(Claim)
  /\ WF_vars(ReturnClaim)
  /\ WF_vars(ReturnRow)
  /\ WF_vars(DesktopRead)

TerminalConverges ==
  \A peer \in ReplicaHolder :
    IsTerminal(reqState[Owner]) ~> (reqState[peer] = reqState[Owner])

DesktopEventuallyVisible ==
  desktopWrote ~> (desktopView = "Visible")

====
