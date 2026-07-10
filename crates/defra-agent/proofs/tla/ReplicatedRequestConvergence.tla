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
(* Design (owner re-drive + reconnect replay, passive peers): the owner     *)
(* re-asserts terminal state for requests it owns through a PERSISTED,      *)
(* bounded per-document budget. A peer that was unavailable through that   *)
(* budget is repaired when its configured replicator reconnects: reinstall *)
(* performs one bounded full replay of the owner's current document DAG.    *)
(* The replay does not author a new request delta, so it does not grow the  *)
(* request's CRDT history. Peers never author lifecycle state.              *)
(*                                                                         *)
(* FIDELITY TO THE SHIPPING CODE. The owner has no back-channel telling it *)
(* whether a peer has caught up, so it CANNOT stop re-emitting "once the   *)
(* peer converges". The code re-asserts each terminal row a fixed          *)
(* TERMINAL_REDRIVE_CAP times and then stops, converged or not. This model *)
(* mirrors that exactly: EmitTerminalDelta is gated ONLY by the request's   *)
(* persisted budget emitCount < Cap, NOT by whether peers already match     *)
(* the owner. emitCount is durable across CrashOwner. TerminalConverges no  *)
(* longer depends on Cap exceeding an unbounded partition: an initially     *)
(* offline peer fairly recovers, then ReplayTerminalSnapshot applies the    *)
(* owner's terminal DAG without consuming another same-value write. The     *)
(* Stuck diagnostic disables reconnect replay and still reaches the old     *)
(* exhausted-budget state, proving replay is load-bearing.                  *)
(***************************************************************************)

CONSTANTS
  Owner,          \* the owning node (the request's agent_did holder)
  ReplicaHolder,  \* the set of non-owning peer replicas (|ReplicaHolder| >= 2)
  DeltaId,        \* bounded pool of terminal-delta ids (PushLog re-emissions)
  MaxDrops,       \* how many terminal deltas the gossip channel may drop
  MaxCrashes,     \* total node crashes across owner + peers
  TerminalKind,   \* the terminal lifecycle states (e.g. Completed, Failed)
  Cap,            \* per-request re-emit budget (shipping TERMINAL_REDRIVE_CAP):
                  \* one owner write fans out to every online replicator;
                  \* it occurs AT MOST Cap times without a convergence back-channel
  ReplayOnRecovery, \* TRUE in production: reconnect forces one full replay
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
ASSUME ReplayOnRecoveryIsBoolean == ReplayOnRecovery \in BOOLEAN
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
  emitCount,       \* 0..Cap — DURABLE per-request same-value writes
  peerOnline,      \* [ReplicaHolder -> BOOLEAN] — an initial partition may be unbounded
  replayPending,   \* [ReplicaHolder -> BOOLEAN] — reconnect replay obligation
  replayCount      \* [ReplicaHolder -> 0..1] — one bounded full replay per recovery

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
  replayCount
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

Init ==
  /\ reqState        = [n \in Node |-> "Pending"]
  /\ messages        = {}
  /\ pendingInbound  = {}
  /\ deltaIdsUsed    = {}
  /\ dropCount       = 0
  /\ crashCount      = 0
  /\ emitCount       = 0
  \* A peer may begin partitioned. Weak fairness on RecoverPeer means the
  \* partition can last arbitrarily long but not forever.
  /\ peerOnline      \in [ReplicaHolder -> BOOLEAN]
  /\ replayPending   = [peer \in ReplicaHolder |-> FALSE]
  /\ replayCount     = [peer \in ReplicaHolder |-> 0]

(***************************************************************************)
(* Owner-only lifecycle. The owner advances its OWN replica through the    *)
(* Pending -> Claimed -> Processing -> terminal chain. Terminal is         *)
(* absorbing: no action moves the owner out of a terminal state.           *)
(***************************************************************************)

Claim ==
  /\ reqState[Owner] = "Pending"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

Process ==
  /\ reqState[Owner] = "Claimed"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Processing"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

Terminalize(k) ==
  /\ k \in TerminalKind
  /\ reqState[Owner] = "Processing"
  /\ reqState' = [reqState EXCEPT ![Owner] = k]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

(***************************************************************************)
(* Owner terminal re-drive. Abstracts the Rust binding (idempotent         *)
(* same-value terminal re-assert) as a delta onto the gossip channel,      *)
(* fanned out to every currently online configured peer.                    *)
(*                                                                         *)
(* Gated by the per-request budget emitCount < Cap — NOT by whether peers   *)
(* peer has converged. This is the crux of fidelity to the shipping code:   *)
(* the owner cannot observe convergence, so it spends a fixed budget of     *)
(* re-emissions per peer and then stops. With Cap large enough to outlast    *)
(* the delivery loss the peer converges; with Cap too small (the Stuck       *)
(* diagnostic) a lost emission strands it.                                   *)
(*                                                                         *)
(* MODELING ASSUMPTION (tick spacing). Also gated on "no delta for this      *)
(* peer already in flight" (AllDeltas). The shipping re-drive re-asserts at   *)
(* most once per 5s reconcile tick, and the ticks are spaced far enough that *)
(* each re-assert's PushLog wave is delivered, dropped, or crash-lost before *)
(* the next — so at most one re-assert wave is outstanding. Without this     *)
(* guard the model would let the owner blast all Cap copies into the channel *)
(* simultaneously and then lose every one to a SINGLE crash (CrashPeer       *)
(* clears the whole pendingInbound at once) — an interleaving the tick-spaced *)
(* code never exhibits. With the guard, each of the Cap re-asserts is an      *)
(* independent attempt that resolves before the next, so the budget genuinely *)
(* buys Cap delivery tries.                                                   *)
(***************************************************************************)

FreshDeltaIds(k) == Cardinality(DeltaId \ deltaIdsUsed) >= k

OnlinePeers == {peer \in ReplicaHolder : peerOnline[peer]}

EmitTerminalDelta ==
  /\ IsTerminal(reqState[Owner])
  /\ emitCount < Cap
  /\ AllDeltas = {}
  /\ FreshDeltaIds(1)
  /\ LET id == CHOOSE i \in DeltaId \ deltaIdsUsed : TRUE
         wave == {[id |-> id, target |-> peer, value |-> reqState[Owner]]
                    : peer \in OnlinePeers}
     IN /\ messages' = messages \cup wave
        /\ deltaIdsUsed' = deltaIdsUsed \cup {id}
        /\ emitCount'    = emitCount + 1
  /\ UNCHANGED <<reqState, pendingInbound, dropCount, crashCount,
                  peerOnline, replayPending, replayCount>>

(***************************************************************************)
(* Gossip channel: deliver / drop (bounded).                               *)
(***************************************************************************)

DeliverDelta(d) ==
  /\ d \in messages
  /\ peerOnline[d.target]
  /\ messages'       = messages \ {d}
  /\ pendingInbound' = pendingInbound \cup {d}
  /\ UNCHANGED <<reqState, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

DropDelta(d) ==
  /\ d \in messages
  /\ dropCount < MaxDrops
  /\ messages'  = messages \ {d}
  /\ dropCount' = dropCount + 1
  /\ UNCHANGED <<reqState, pendingInbound, deltaIdsUsed, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

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
  /\ UNCHANGED <<messages, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

(***************************************************************************)
(* A configured peer that was unavailable through the bounded blind budget *)
(* eventually reconnects. Reinstalling its existing filtered replicator     *)
(* performs one full replay of current owner-authored DAG heads. The replay  *)
(* changes no owner request field and consumes no emitCount/delta id.        *)
(***************************************************************************)

RecoverPeer(peer) ==
  /\ ReplayOnRecovery
  /\ peer \in ReplicaHolder
  /\ ~peerOnline[peer]
  /\ peerOnline'    = [peerOnline EXCEPT ![peer] = TRUE]
  /\ replayPending' = [replayPending EXCEPT ![peer] = TRUE]
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount,
                  crashCount, emitCount, replayCount>>

ReplayTerminalSnapshot(peer) ==
  /\ ReplayOnRecovery
  /\ peer \in ReplicaHolder
  /\ peerOnline[peer]
  /\ replayPending[peer]
  /\ IsTerminal(reqState[Owner])
  /\ reqState'      = [reqState EXCEPT ![peer] = reqState[Owner]]
  /\ replayPending' = [replayPending EXCEPT ![peer] = FALSE]
  /\ replayCount'   = [replayCount EXCEPT ![peer] = 1]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount,
                  emitCount, peerOnline>>

(***************************************************************************)
(* Crash abstraction. A peer crash loses its volatile (not-yet-persisted)  *)
(* inbound deltas; owner-durable terminal state survives an owner restart. *)
(***************************************************************************)

CrashPeer(peer) ==
  /\ peer \in ReplicaHolder
  /\ crashCount < MaxCrashes
  /\ pendingInbound' = {d \in pendingInbound : d.target # peer}
  /\ crashCount'     = crashCount + 1
  /\ UNCHANGED <<reqState, messages, deltaIdsUsed, dropCount, emitCount,
                  peerOnline, replayPending, replayCount>>

CrashOwner ==
  /\ crashCount < MaxCrashes
  /\ crashCount' = crashCount + 1
  \* The persisted re-drive count survives owner restart.
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, emitCount,
                  peerOnline, replayPending, replayCount>>

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
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emitCount,
                  peerOnline, replayPending, replayCount>>

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

\* The total same-value request writes are bounded by the persisted request
\* budget. One write may fan out a delivery delta to every online peer.
StateBound ==
  Cardinality(deltaIdsUsed) <= Cap

ReplayBound ==
  \A peer \in ReplicaHolder : replayCount[peer] <= 1

(***************************************************************************)
(* Transitions and fairness.                                               *)
(***************************************************************************)

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
  \/ \E peer \in ReplicaHolder : PeerClaimsForeign(peer)
  \/ \E peer \in ReplicaHolder : CrashPeer(peer)
  \/ CrashOwner

\* Weak fairness on the re-drive workers only: the owner keeps re-emitting the
\* terminal delta until its persisted per-request budget is spent, and delivery/persistence
\* make progress. No fairness on the owner lifecycle (Claim/Process/Terminalize),
\* drops, or crashes — those are voluntary. Convergence is therefore driven by
\* the re-emit BUDGET, not by fairness alone: once emitCount = Cap the
\* re-emit is disabled, so if the budget is smaller than the loss a peer
\* suffers, fairness cannot rescue it (the Stuck diagnostic).
Fairness ==
  /\ WF_vars(EmitTerminalDelta)
  /\ \A peer \in ReplicaHolder : WF_vars(RecoverPeer(peer))
  /\ \A peer \in ReplicaHolder : WF_vars(ReplayTerminalSnapshot(peer))
  /\ WF_vars(\E d \in messages : DeliverDelta(d))
  /\ WF_vars(\E d \in pendingInbound : PersistDeltaOnPeer(d))

Spec == Init /\ [][Next]_vars /\ Fairness

(***************************************************************************)
(* Liveness: owner-terminal converges to every replica holder. Online peers *)
(* converge through the bounded re-drive when its budget outlasts bounded   *)
(* delivery loss. A peer partitioned beyond the entire cap converges after  *)
(* fair recovery through one bounded full replay. The Stuck diagnostic      *)
(* disables ReplayOnRecovery and retains the pre-fix violation.             *)
(***************************************************************************)

TerminalConverges ==
  \A peer \in ReplicaHolder :
    IsTerminal(reqState[Owner]) ~> (reqState[peer] = reqState[Owner])

====
