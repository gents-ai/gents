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

Next == UNCHANGED vars

Spec == Init /\ [][Next]_vars

====
