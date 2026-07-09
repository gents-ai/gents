---- MODULE ReplicatedRequestConvergence ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Replicated AgentRequest terminal-state convergence (#664).              *)
(*                                                                         *)
(* Spec design:                                                            *)
(*   docs/superpowers/specs/2026-07-08-replicated-request-convergence-     *)
(*   664-design.md                                                         *)
(*                                                                         *)
(* Under subagent-host replication one AgentRequest document is replicated *)
(* onto non-owning peer nodes. SAFETY already holds: the watcher filters   *)
(* agent_did == self on both claim seams (watcher/query.rs), so a peer     *)
(* never claims or processes a foreign replica. LIVENESS did not: the      *)
(* owner's terminal delta could drop and never converge to the peers,      *)
(* because DefraDB has no per-doc anti-entropy re-drive on a running peer   *)
(* (defradb.rs#1074) and recovery is owner-scoped + startup-only.          *)
(*                                                                         *)
(* Design (owner re-drive, passive peers): the owner periodically          *)
(* re-asserts terminal state for requests it owns, forcing the terminal    *)
(* delta back through the normal PushLog path. This model abstracts the    *)
(* Phase-0 Rust binding (same-value re-assert) as a re-emittable           *)
(* EmitTerminalDelta action. Peers are strictly passive: their only        *)
(* lifecycle action is applying a delivered owner delta.                   *)
(*                                                                         *)
(* FIDELITY TO THE SHIPPING CODE. The owner has no back-channel telling it *)
(* whether a peer has caught up, so it CANNOT stop re-emitting "once the   *)
(* peer converges". The code re-asserts each terminal row a fixed          *)
(* TERMINAL_REDRIVE_CAP times and then stops, converged or not. This model *)
(* mirrors that exactly: EmitTerminalDelta is gated ONLY by a per-peer      *)
(* budget emitCount[peer] < Cap, NOT by whether the peer already matches    *)
(* the owner. Consequently TerminalConverges is a CONDITIONAL theorem:      *)
(* it holds iff the budget Cap exceeds the delivery loss a peer suffers     *)
(* (MaxDrops + MaxCrashes). The green config takes Cap = 3 (the shipping    *)
(* cap) with one drop + one crash and converges; the Stuck config takes     *)
(* Cap = 1 and does not (a lost single emission strands the peer). Beyond   *)
(* the budget the code relies on the next organic write to the row — that   *)
(* fallback is out of this model's scope.                                   *)
(***************************************************************************)

CONSTANTS
  Owner,          \* the owning node (the request's agent_did holder)
  ReplicaHolder,  \* the set of non-owning peer replicas (|ReplicaHolder| >= 2)
  DeltaId,        \* bounded pool of terminal-delta ids (PushLog re-emissions)
  MaxDrops,       \* how many terminal deltas the gossip channel may drop
  MaxCrashes,     \* total node crashes across owner + peers
  TerminalKind,   \* the terminal lifecycle states (e.g. Completed, Failed)
  Cap,            \* per-peer re-emit budget (the shipping TERMINAL_REDRIVE_CAP):
                  \* the owner re-asserts each peer's terminal AT MOST Cap times,
                  \* WITHOUT observing whether the peer converged (no back-channel)
  AllowPeerClaim  \* FALSE normally; TRUE arms the adversarial peer-claim action
                  \* (the diagnostic that proves SingleClaimer is falsifiable)

Node == {Owner} \cup ReplicaHolder

NonTerminal == {"Pending", "Claimed", "Processing"}
LifecycleState == NonTerminal \cup TerminalKind

ASSUME OwnerNotAReplica == Owner \notin ReplicaHolder
ASSUME ReplicaHolderIsFiniteSet == IsFiniteSet(ReplicaHolder)
ASSUME AtLeastTwoReplicas == Cardinality(ReplicaHolder) >= 2
ASSUME DeltaIdIsFiniteSet == IsFiniteSet(DeltaId)
ASSUME MaxDropsIsNat == MaxDrops \in Nat
ASSUME MaxCrashesIsNat == MaxCrashes \in Nat
ASSUME TerminalKindNonEmpty == TerminalKind # {}
ASSUME TerminalKindDisjoint == TerminalKind \cap NonTerminal = {}
ASSUME CapIsPositiveNat == Cap \in (Nat \ {0})
ASSUME AllowPeerClaimIsBoolean == AllowPeerClaim \in BOOLEAN

Delta == [
  id     : DeltaId,
  target : ReplicaHolder,
  value  : TerminalKind
]

VARIABLES
  reqState,        \* [Node -> LifecycleState] — each node's local view of the request
  messages,        \* SUBSET Delta — terminal deltas in the gossip channel
  pendingInbound,  \* SUBSET Delta — deltas delivered to a peer, not yet persisted
  deltaIdsUsed,    \* SUBSET DeltaId — ids consumed by re-emissions
  dropCount,       \* 0..MaxDrops — deltas dropped so far
  crashCount,      \* 0..MaxCrashes — node crashes so far
  emitCount        \* [ReplicaHolder -> 0..Cap] — terminal deltas emitted per peer

vars == <<
  reqState,
  messages,
  pendingInbound,
  deltaIdsUsed,
  dropCount,
  crashCount,
  emitCount
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
  /\ emitCount       \in [ReplicaHolder -> 0..Cap]

Init ==
  /\ reqState        = [n \in Node |-> "Pending"]
  /\ messages        = {}
  /\ pendingInbound  = {}
  /\ deltaIdsUsed    = {}
  /\ dropCount       = 0
  /\ crashCount      = 0
  /\ emitCount       = [peer \in ReplicaHolder |-> 0]

(***************************************************************************)
(* Owner-only lifecycle. The owner advances its OWN replica through the    *)
(* Pending -> Claimed -> Processing -> terminal chain. Terminal is         *)
(* absorbing: no action moves the owner out of a terminal state.           *)
(***************************************************************************)

Claim ==
  /\ reqState[Owner] = "Pending"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount>>

Process ==
  /\ reqState[Owner] = "Claimed"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Processing"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount>>

Terminalize(k) ==
  /\ k \in TerminalKind
  /\ reqState[Owner] = "Processing"
  /\ reqState' = [reqState EXCEPT ![Owner] = k]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount>>

(***************************************************************************)
(* Owner terminal re-drive. Abstracts the Rust binding (idempotent         *)
(* same-value terminal re-assert) as a delta onto the gossip channel,      *)
(* targeted at one peer.                                                    *)
(*                                                                         *)
(* Gated by the per-peer budget emitCount[peer] < Cap — NOT by whether the  *)
(* peer has converged. This is the crux of fidelity to the shipping code:   *)
(* the owner cannot observe convergence, so it spends a fixed budget of     *)
(* re-emissions per peer and then stops. With Cap large enough to outlast    *)
(* the delivery loss the peer converges; with Cap too small (the Stuck       *)
(* diagnostic) a lost emission strands it.                                   *)
(*                                                                         *)
(* MODELING ASSUMPTION (tick spacing). Also gated on "no delta for this      *)
(* peer already in flight" (AllDeltas). The shipping re-drive re-asserts at   *)
(* most once per 5s reconcile tick, and the ticks are spaced far enough that *)
(* each re-assert's PushLog is delivered, dropped, or crash-lost before the  *)
(* next — so at most one re-assert per peer is ever outstanding. Without this *)
(* guard the model would let the owner blast all Cap copies into the channel *)
(* simultaneously and then lose every one to a SINGLE crash (CrashPeer       *)
(* clears the whole pendingInbound at once) — an interleaving the tick-spaced *)
(* code never exhibits. With the guard, each of the Cap re-asserts is an      *)
(* independent attempt that resolves before the next, so the budget genuinely *)
(* buys Cap delivery tries.                                                   *)
(***************************************************************************)

FreshDeltaIds(k) == Cardinality(DeltaId \ deltaIdsUsed) >= k

PeerHasInflightDelta(peer) == \E d \in AllDeltas : d.target = peer

EmitTerminalDelta(peer) ==
  /\ peer \in ReplicaHolder
  /\ IsTerminal(reqState[Owner])
  /\ emitCount[peer] < Cap
  /\ ~PeerHasInflightDelta(peer)
  /\ FreshDeltaIds(1)
  /\ LET id == CHOOSE i \in DeltaId \ deltaIdsUsed : TRUE
         d  == [id |-> id, target |-> peer, value |-> reqState[Owner]]
     IN /\ messages'     = messages \cup {d}
        /\ deltaIdsUsed' = deltaIdsUsed \cup {id}
        /\ emitCount'    = [emitCount EXCEPT ![peer] = @ + 1]
  /\ UNCHANGED <<reqState, pendingInbound, dropCount, crashCount>>

(***************************************************************************)
(* Gossip channel: deliver / drop (bounded).                               *)
(***************************************************************************)

DeliverDelta(d) ==
  /\ d \in messages
  /\ messages'       = messages \ {d}
  /\ pendingInbound' = pendingInbound \cup {d}
  /\ UNCHANGED <<reqState, deltaIdsUsed, dropCount, crashCount, emitCount>>

DropDelta(d) ==
  /\ d \in messages
  /\ dropCount < MaxDrops
  /\ messages'  = messages \ {d}
  /\ dropCount' = dropCount + 1
  /\ UNCHANGED <<reqState, pendingInbound, deltaIdsUsed, crashCount, emitCount>>

(***************************************************************************)
(* Peer applies a delivered owner delta. This is the ONLY action that      *)
(* transitions a peer's reqState — peers never claim or process. The value *)
(* is always the owner's terminal, so a peer converges Pending -> terminal *)
(* and never regresses.                                                    *)
(***************************************************************************)

PersistDeltaOnPeer(d) ==
  /\ d \in pendingInbound
  /\ pendingInbound' = pendingInbound \ {d}
  /\ reqState'       = [reqState EXCEPT ![d.target] = d.value]
  /\ UNCHANGED <<messages, deltaIdsUsed, dropCount, crashCount, emitCount>>

(***************************************************************************)
(* Crash abstraction. A peer crash loses its volatile (not-yet-persisted)  *)
(* inbound deltas; owner-durable terminal state survives an owner restart. *)
(***************************************************************************)

CrashPeer(peer) ==
  /\ peer \in ReplicaHolder
  /\ crashCount < MaxCrashes
  /\ pendingInbound' = {d \in pendingInbound : d.target # peer}
  /\ crashCount'     = crashCount + 1
  /\ UNCHANGED <<reqState, messages, deltaIdsUsed, dropCount, emitCount>>

CrashOwner ==
  /\ crashCount < MaxCrashes
  /\ crashCount' = crashCount + 1
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, emitCount>>

(***************************************************************************)
(* Adversarial peer claim (diagnostic). A peer transitions its OWN replica  *)
(* into Claimed of its own volition — exactly what the watcher agent_did     *)
(* filter forbids, and what a peer-side write to a foreign replica would do. *)
(* Disabled (AllowPeerClaim = FALSE) in the real specs; armed only in the    *)
(* MCReplicatedRequestConvergencePeerClaim diagnostic, where it makes        *)
(* SingleClaimer reachably VIOLATED — proving clause (1) is not vacuous.     *)
(***************************************************************************)

PeerClaimsForeign(peer) ==
  /\ AllowPeerClaim
  /\ peer \in ReplicaHolder
  /\ reqState[peer] = "Pending"
  /\ reqState' = [reqState EXCEPT ![peer] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount>>

(***************************************************************************)
(* Safety invariants.                                                      *)
(***************************************************************************)

\* SingleClaimer: peers are strictly passive. A non-owner never sits in a
\* claimed/processing state (that would require it to claim of its own
\* volition — the exact thing the agent_did watcher filter forbids), and any
\* terminal a peer holds is the owner's terminal, delivered by an owner delta.
\* Clause (1) is NOT vacuous: the PeerClaimsForeign action (armed via
\* AllowPeerClaim in the MC...PeerClaim diagnostic) drives a peer into Claimed
\* and reachably violates it — so the green run is evidence the fence holds,
\* not a type artifact.
\*
\* MODELING ASSUMPTION (scope). This model abstracts a peer replica as receiving
\* only the owner's TERMINAL deltas, so a modeled peer is only ever Pending or a
\* delivered terminal. In production a peer replica also carries the owner's
\* replicated INTERMEDIATE states (Claimed/Processing) — that is the literal
\* #661 shape (peers stuck at "processing"). Those are benign (owner-originated,
\* never self-claimed) and orthogonal to the terminal-convergence property under
\* study, so they are not modeled here; SingleClaimer proves the property that
\* matters — a peer never self-originates a claim — not that a peer is never
\* observed in an intermediate state. Modeling intermediate-state replication
\* (with CRDT priority ordering) is a separate fidelity extension.
SingleClaimer ==
  \A n \in ReplicaHolder :
    /\ reqState[n] \notin {"Claimed", "Processing"}
    /\ (IsTerminal(reqState[n]) =>
          /\ IsTerminal(reqState[Owner])
          /\ reqState[n] = reqState[Owner])

\* Every in-flight delta is backed by the owner's (absorbing) terminal value.
DeltaBackedByOwnerTerminal ==
  \A d \in AllDeltas :
    /\ IsTerminal(reqState[Owner])
    /\ d.value = reqState[Owner]

\* Every in-flight delta's id is recorded (no id reuse).
DeltaIdsTracked ==
  \A d \in AllDeltas : d.id \in deltaIdsUsed

\* The total re-emissions are bounded by the per-peer budget: emitCount already
\* caps each peer at Cap, so deltaIdsUsed can never exceed Cap * |ReplicaHolder|.
StateBound ==
  Cardinality(deltaIdsUsed) <= Cap * Cardinality(ReplicaHolder)

(***************************************************************************)
(* Transitions and fairness.                                               *)
(***************************************************************************)

Next ==
  \/ Claim
  \/ Process
  \/ \E k \in TerminalKind : Terminalize(k)
  \/ \E peer \in ReplicaHolder : EmitTerminalDelta(peer)
  \/ \E d \in messages : DeliverDelta(d)
  \/ \E d \in messages : DropDelta(d)
  \/ \E d \in pendingInbound : PersistDeltaOnPeer(d)
  \/ \E peer \in ReplicaHolder : PeerClaimsForeign(peer)
  \/ \E peer \in ReplicaHolder : CrashPeer(peer)
  \/ CrashOwner

\* Weak fairness on the re-drive workers only: the owner keeps re-emitting the
\* terminal delta until its per-peer budget is spent, and delivery/persistence
\* make progress. No fairness on the owner lifecycle (Claim/Process/Terminalize),
\* drops, or crashes — those are voluntary. Convergence is therefore driven by
\* the re-emit BUDGET, not by fairness alone: once emitCount[peer] = Cap the
\* re-emit is disabled, so if the budget is smaller than the loss a peer
\* suffers, fairness cannot rescue it (the Stuck diagnostic).
Fairness ==
  /\ \A peer \in ReplicaHolder : WF_vars(EmitTerminalDelta(peer))
  /\ WF_vars(\E d \in messages : DeliverDelta(d))
  /\ WF_vars(\E d \in pendingInbound : PersistDeltaOnPeer(d))

Spec == Init /\ [][Next]_vars /\ Fairness

(***************************************************************************)
(* Liveness: owner-terminal converges to every replica holder — CONDITIONAL *)
(* on the re-emit budget Cap exceeding the delivery loss (MaxDrops +         *)
(* MaxCrashes). Holds in the green config (Cap = 3 > 1 drop + 1 crash);      *)
(* reachably violated in the Stuck config (Cap = 1 <= the loss).            *)
(***************************************************************************)

TerminalConverges ==
  \A peer \in ReplicaHolder :
    IsTerminal(reqState[Owner]) ~> (reqState[peer] = reqState[Owner])

====
