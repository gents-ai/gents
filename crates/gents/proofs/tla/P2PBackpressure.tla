---- MODULE P2PBackpressure ----
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
  Peer,
  ResponsivePeer,
  InboundPeer,
  PushWorkers,
  MaxPending,
  TimeoutReleasesSlot,
  AckWithoutPendingAllowed

ASSUME PeerFinite == IsFiniteSet(Peer)
ASSUME ResponsiveSubset == ResponsivePeer \subseteq Peer
ASSUME InboundSubset == InboundPeer \subseteq Peer
ASSUME PushWorkersPositive == PushWorkers \in Nat /\ PushWorkers > 0
ASSUME MaxPendingNatural == MaxPending \in Nat
ASSUME TimeoutReleasesSlotBool == TimeoutReleasesSlot \in BOOLEAN
ASSUME AckWithoutPendingAllowedBool == AckWithoutPendingAllowed \in BOOLEAN

VARIABLES
  outState,
  pending,
  merged,
  acked,
  nacked

vars == <<outState, pending, merged, acked, nacked>>

OutStates == {"Queued", "InFlight", "Delivered", "Failed"}

InFlightPeers == {p \in Peer : outState[p] = "InFlight"}

InboundUnsettled(p) ==
  /\ p \in InboundPeer
  /\ p \notin acked
  /\ p \notin nacked

TypeOK ==
  /\ outState \in [Peer -> OutStates]
  /\ pending \subseteq InboundPeer
  /\ merged \subseteq InboundPeer
  /\ acked \subseteq InboundPeer
  /\ nacked \subseteq InboundPeer

Init ==
  /\ outState = [p \in Peer |-> "Queued"]
  /\ pending = {}
  /\ merged = {}
  /\ acked = {}
  /\ nacked = {}

StartPush(p) ==
  /\ p \in Peer
  /\ outState[p] = "Queued"
  /\ Cardinality(InFlightPeers) < PushWorkers
  /\ outState' = [outState EXCEPT ![p] = "InFlight"]
  /\ UNCHANGED <<pending, merged, acked, nacked>>

PushSucceeds(p) ==
  /\ p \in ResponsivePeer
  /\ outState[p] = "InFlight"
  /\ outState' = [outState EXCEPT ![p] = "Delivered"]
  /\ UNCHANGED <<pending, merged, acked, nacked>>

PushTimesOut(p) ==
  /\ p \in (Peer \ ResponsivePeer)
  /\ outState[p] = "InFlight"
  /\ TimeoutReleasesSlot
  /\ outState' = [outState EXCEPT ![p] = "Failed"]
  /\ UNCHANGED <<pending, merged, acked, nacked>>

ReceiveComplete(p) ==
  /\ InboundUnsettled(p)
  /\ merged' = merged \cup {p}
  /\ acked' = acked \cup {p}
  /\ UNCHANGED <<outState, pending, nacked>>

ReceiveMissingAndRegister(p) ==
  /\ InboundUnsettled(p)
  /\ Cardinality(pending) < MaxPending
  /\ pending' = pending \cup {p}
  /\ acked' = acked \cup {p}
  /\ UNCHANGED <<outState, merged, nacked>>

NackAtCapacity(p) ==
  /\ InboundUnsettled(p)
  /\ Cardinality(pending) >= MaxPending
  /\ nacked' = nacked \cup {p}
  /\ UNCHANGED <<outState, pending, merged, acked>>

BadAckAtCapacity(p) ==
  /\ AckWithoutPendingAllowed
  /\ InboundUnsettled(p)
  /\ Cardinality(pending) >= MaxPending
  /\ acked' = acked \cup {p}
  /\ UNCHANGED <<outState, pending, merged, nacked>>

SettleInbound(p) ==
  \/ ReceiveComplete(p)
  \/ ReceiveMissingAndRegister(p)
  \/ NackAtCapacity(p)
  \/ BadAckAtCapacity(p)

ResolvePending(p) ==
  /\ p \in pending
  /\ pending' = pending \ {p}
  /\ merged' = merged \cup {p}
  /\ UNCHANGED <<outState, acked, nacked>>

Next ==
  \/ \E p \in Peer : StartPush(p)
  \/ \E p \in Peer : PushSucceeds(p)
  \/ \E p \in Peer : PushTimesOut(p)
  \/ \E p \in InboundPeer : SettleInbound(p)
  \/ \E p \in InboundPeer : ResolvePending(p)

Fairness ==
  /\ \A p \in Peer : SF_vars(StartPush(p))
  /\ \A p \in ResponsivePeer : WF_vars(PushSucceeds(p))
  /\ \A p \in (Peer \ ResponsivePeer) : WF_vars(PushTimesOut(p))
  /\ \A p \in InboundPeer : WF_vars(SettleInbound(p))
  /\ \A p \in InboundPeer : WF_vars(ResolvePending(p))

Spec == Init /\ [][Next]_vars /\ Fairness

PushSlotsBounded ==
  Cardinality(InFlightPeers) <= PushWorkers

PendingBounded ==
  Cardinality(pending) <= MaxPending

SuccessAckBacked ==
  \A p \in InboundPeer : p \in acked => (p \in pending \/ p \in merged)

FailedOnlyUnresponsive ==
  \A p \in Peer : outState[p] = "Failed" => p \notin ResponsivePeer

HealthyPeersDeliver ==
  \A p \in ResponsivePeer : <> (outState[p] = "Delivered")

InboundSettles ==
  \A p \in InboundPeer : <> (p \in acked \/ p \in nacked)
====
