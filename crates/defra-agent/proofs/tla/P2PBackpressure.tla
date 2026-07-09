---- MODULE P2PBackpressure ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Hub fan-in / fan-out admission model for issue #630.                    *)
(*                                                                         *)
(* PairingTransport.tla models one directed edge getting connected and     *)
(* installable. This model starts later in the pipeline: the hub already   *)
(* has replicator peers, local writes create PushLog work, and remote      *)
(* peers can also push DAG roots into the hub.                              *)
(*                                                                         *)
(* It captures two production requirements that showed up in the Amy       *)
(* v0.6.6 report:                                                          *)
(*                                                                         *)
(*   1. Outbound push workers are bounded. A nonresponsive peer must       *)
(*      eventually release its worker slot by timeout/failure; otherwise a *)
(*      small semaphore can be filled by stuck sends and healthy peers can *)
(*      stop receiving updates.                                            *)
(*                                                                         *)
(*   2. Inbound successful PushLog replies must be backed by durable work: *)
(*      either the block merged, or the DAG root is registered in the       *)
(*      pending-DAG map. If the map is full, the hub must nack/backpressure *)
(*      rather than success-ack and drop tracking state.                    *)
(*                                                                         *)
(* The model is intentionally finite: each peer has one outbound update and *)
(* each inbound peer sends one DAG. Repeated organic writes/retry loops,    *)
(* multi-wave saturation, Bitswap stall lifetime, rate-limit token buckets, *)
(* and gossip send-loop health are outside this first gate.                 *)
(*                                                                          *)
(* Pending capacity is keyed by peer here as a one-wave abstraction;        *)
(* production pending-DAG capacity is keyed by DAG root / CID (one peer     *)
(* may hold many pending roots). The properties below are the local         *)
(* admission obligations that make continuous retry loops safe — not a      *)
(* proof that a live hub under Amy-class multi-wave load stays healthy.     *)
(*                                                                          *)
(* PushWorkers maps to SyncConfig.max_concurrent_push_tasks /               *)
(* P2PConfig.max_concurrent_push_tasks (operator-tunable on server via      *)
(* --p2p-max-concurrent-push-tasks). TimeoutReleasesSlot models whether a   *)
(* timed-out PushLog frees its semaphore permit — true in the green path.   *)
(***************************************************************************)

CONSTANTS
  Peer,                    \* finite set of replicator peers
  ResponsivePeer,          \* peers whose PushLog request can succeed
  InboundPeer,             \* peers that send a PushLog into this hub
  PushWorkers,             \* outbound push semaphore capacity
  MaxPending,              \* inbound pending-DAG capacity
  TimeoutReleasesSlot,     \* BOOLEAN: does timeout/failure free a push slot?
  AckWithoutPendingAllowed \* BOOLEAN: diagnostic switch for the bad ack bug

ASSUME PeerFinite == IsFiniteSet(Peer)
ASSUME ResponsiveSubset == ResponsivePeer \subseteq Peer
ASSUME InboundSubset == InboundPeer \subseteq Peer
ASSUME PushWorkersPositive == PushWorkers \in Nat /\ PushWorkers > 0
ASSUME MaxPendingNatural == MaxPending \in Nat
ASSUME TimeoutReleasesSlotBool == TimeoutReleasesSlot \in BOOLEAN
ASSUME AckWithoutPendingAllowedBool == AckWithoutPendingAllowed \in BOOLEAN

VARIABLES
  outState, \* [Peer -> {"Queued","InFlight","Delivered","Failed"}]
  pending,  \* inbound roots registered for later DAG completion
  merged,   \* inbound roots already complete/merged
  acked,    \* inbound PushLog success replies
  nacked    \* inbound PushLog backpressure/error replies

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

(***************************************************************************)
(* Outbound push fan-out.                                                  *)
(***************************************************************************)

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

(***************************************************************************)
(* Inbound PushLog admission.                                              *)
(***************************************************************************)

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

\* Diagnostic bad behavior: success-ack at capacity without registering the
\* DAG. This is the bug the production PushLog reply invariant is meant to
\* forbid.
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

(***************************************************************************)
(* Fairness.                                                              *)
(*                                                                         *)
(* StartPush uses strong fairness because semaphore availability can be    *)
(* intermittent. Success/timeout/settle/resolve actions use weak fairness  *)
(* once their guards continuously hold. There is deliberately no fairness  *)
(* that can make a nonresponsive peer succeed or make a disabled timeout   *)
(* release a slot.                                                         *)
(***************************************************************************)

Fairness ==
  /\ \A p \in Peer : SF_vars(StartPush(p))
  /\ \A p \in ResponsivePeer : WF_vars(PushSucceeds(p))
  /\ \A p \in (Peer \ ResponsivePeer) : WF_vars(PushTimesOut(p))
  /\ \A p \in InboundPeer : WF_vars(SettleInbound(p))
  /\ \A p \in InboundPeer : WF_vars(ResolvePending(p))

Spec == Init /\ [][Next]_vars /\ Fairness

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)

PushSlotsBounded ==
  Cardinality(InFlightPeers) <= PushWorkers

PendingBounded ==
  Cardinality(pending) <= MaxPending

\* A success reply cannot discard work. While pending, the DAG is tracked;
\* after resolution, it is merged. Nacks are not success replies and do not
\* have to satisfy this invariant.
SuccessAckBacked ==
  \A p \in InboundPeer : p \in acked => (p \in pending \/ p \in merged)

FailedOnlyUnresponsive ==
  \A p \in Peer : outState[p] = "Failed" => p \notin ResponsivePeer

HealthyPeersDeliver ==
  \A p \in ResponsivePeer : <> (outState[p] = "Delivered")

InboundSettles ==
  \A p \in InboundPeer : <> (p \in acked \/ p \in nacked)
====
