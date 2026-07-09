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
(* Phase-0 Rust binding (same-value re-assert / convergence_seq bump) as a *)
(* single re-emittable EmitTerminalDelta action. Peers are strictly        *)
(* passive: their only lifecycle action is applying a delivered owner       *)
(* delta.                                                                   *)
(***************************************************************************)

CONSTANTS
  Owner,          \* the owning node (the request's agent_did holder)
  ReplicaHolder,  \* the set of non-owning peer replicas (|ReplicaHolder| >= 2)
  DeltaId,        \* bounded pool of terminal-delta ids (PushLog re-emissions)
  MaxDrops,       \* how many terminal deltas the gossip channel may drop
  MaxCrashes,     \* total node crashes across owner + peers
  TerminalKind,   \* the terminal lifecycle states (e.g. Completed, Failed)
  Reemit,         \* TRUE: owner re-drive enabled; FALSE: single-shot (diagnostic)
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
ASSUME ReemitIsBoolean == Reemit \in BOOLEAN
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
  emittedTargets   \* SUBSET ReplicaHolder — peers a delta has ever been emitted for

vars == <<
  reqState,
  messages,
  pendingInbound,
  deltaIdsUsed,
  dropCount,
  crashCount,
  emittedTargets
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
  /\ emittedTargets  \in SUBSET ReplicaHolder

Init ==
  /\ reqState        = [n \in Node |-> "Pending"]
  /\ messages        = {}
  /\ pendingInbound  = {}
  /\ deltaIdsUsed    = {}
  /\ dropCount       = 0
  /\ crashCount      = 0
  /\ emittedTargets  = {}

(***************************************************************************)
(* Owner-only lifecycle. The owner advances its OWN replica through the    *)
(* Pending -> Claimed -> Processing -> terminal chain. Terminal is         *)
(* absorbing: no action moves the owner out of a terminal state.           *)
(***************************************************************************)

Claim ==
  /\ reqState[Owner] = "Pending"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Claimed"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emittedTargets>>

Process ==
  /\ reqState[Owner] = "Claimed"
  /\ reqState' = [reqState EXCEPT ![Owner] = "Processing"]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emittedTargets>>

Terminalize(k) ==
  /\ k \in TerminalKind
  /\ reqState[Owner] = "Processing"
  /\ reqState' = [reqState EXCEPT ![Owner] = k]
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emittedTargets>>

(***************************************************************************)
(* Owner terminal re-drive. Abstracts the Rust binding (idempotent         *)
(* same-value terminal re-assert / convergence_seq bump) as a re-emittable *)
(* delta onto the gossip channel, targeted at one peer.                    *)
(*                                                                         *)
(* Enabled while the owner is terminal and the peer has not yet converged. *)
(* When Reemit = TRUE this is the anti-entropy re-drive: it may fire again  *)
(* after a drop or a peer crash. When Reemit = FALSE it is single-shot per  *)
(* peer (the pre-fix behavior: one PushLog delivery, no re-drive) — the     *)
(* diagnostic that makes TerminalConverges reachable-violated.             *)
(***************************************************************************)

FreshDeltaIds(k) == Cardinality(DeltaId \ deltaIdsUsed) >= k

EmitTerminalDelta(peer) ==
  /\ peer \in ReplicaHolder
  /\ IsTerminal(reqState[Owner])
  /\ reqState[peer] # reqState[Owner]        \* bounded re-drive: stop once converged
  /\ (Reemit \/ peer \notin emittedTargets)  \* Reemit=FALSE => at most one emission per peer
  /\ FreshDeltaIds(1)
  /\ LET id == CHOOSE i \in DeltaId \ deltaIdsUsed : TRUE
         d  == [id |-> id, target |-> peer, value |-> reqState[Owner]]
     IN /\ messages'       = messages \cup {d}
        /\ deltaIdsUsed'   = deltaIdsUsed \cup {id}
        /\ emittedTargets' = emittedTargets \cup {peer}
  /\ UNCHANGED <<reqState, pendingInbound, dropCount, crashCount>>

(***************************************************************************)
(* Gossip channel: deliver / drop (bounded).                               *)
(***************************************************************************)

DeliverDelta(d) ==
  /\ d \in messages
  /\ messages'       = messages \ {d}
  /\ pendingInbound' = pendingInbound \cup {d}
  /\ UNCHANGED <<reqState, deltaIdsUsed, dropCount, crashCount, emittedTargets>>

DropDelta(d) ==
  /\ d \in messages
  /\ dropCount < MaxDrops
  /\ messages'  = messages \ {d}
  /\ dropCount' = dropCount + 1
  /\ UNCHANGED <<reqState, pendingInbound, deltaIdsUsed, crashCount, emittedTargets>>

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
  /\ UNCHANGED <<messages, deltaIdsUsed, dropCount, crashCount, emittedTargets>>

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
  /\ UNCHANGED <<messages, pendingInbound, deltaIdsUsed, dropCount, crashCount, emittedTargets>>

(***************************************************************************)
(* Crash abstraction. A peer crash loses its volatile (not-yet-persisted)  *)
(* inbound deltas; owner-durable terminal state survives an owner restart. *)
(***************************************************************************)

CrashPeer(peer) ==
  /\ peer \in ReplicaHolder
  /\ crashCount < MaxCrashes
  /\ pendingInbound' = {d \in pendingInbound : d.target # peer}
  /\ crashCount'     = crashCount + 1
  /\ UNCHANGED <<reqState, messages, deltaIdsUsed, dropCount, emittedTargets>>

CrashOwner ==
  /\ crashCount < MaxCrashes
  /\ crashCount' = crashCount + 1
  /\ UNCHANGED <<reqState, messages, pendingInbound, deltaIdsUsed, dropCount, emittedTargets>>

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

StateBound ==
  Cardinality(deltaIdsUsed) <= 4

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

\* Weak fairness on the re-drive workers only: the owner keeps re-emitting
\* the terminal delta, and delivery/persistence make progress. No fairness on
\* the owner lifecycle (Claim/Process/Terminalize), drops, or crashes — those
\* are voluntary. This is exactly what makes the re-emit load-bearing: bounded
\* drops/crashes cannot starve convergence only because Emit is re-emittable
\* AND fair.
Fairness ==
  /\ \A peer \in ReplicaHolder : WF_vars(EmitTerminalDelta(peer))
  /\ WF_vars(\E d \in messages : DeliverDelta(d))
  /\ WF_vars(\E d \in pendingInbound : PersistDeltaOnPeer(d))

Spec == Init /\ [][Next]_vars /\ Fairness

(***************************************************************************)
(* Liveness: owner-terminal converges to every replica holder.             *)
(***************************************************************************)

TerminalConverges ==
  \A peer \in ReplicaHolder :
    IsTerminal(reqState[Owner]) ~> (reqState[peer] = reqState[Owner])

====
