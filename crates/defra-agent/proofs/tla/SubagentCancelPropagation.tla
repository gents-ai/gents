---- MODULE SubagentCancelPropagation ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Cross-deployment subagent cascade-cancel propagation.                   *)
(*                                                                         *)
(* Spec design:                                                            *)
(*   docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-        *)
(*   tla-design.md                                                         *)
(*                                                                         *)
(* The local Lean bridge transition is abstracted as durable A-side        *)
(* cancel intent. This module verifies delivery to the B-side child        *)
(* request owner over a lossy, crash-prone request-response channel.       *)
(***************************************************************************)

CONSTANTS
  Deployment,
  ParentDeployment,
  ChildDeployment,
  Child,
  RPCId,
  MaxCrashes,
  MaxDrops,
  NoOf

ChildState == {"Running", "Completed", "Failed", "Dead", "Superseded", "Interrupted"}
NaturalTerminal == {"Completed", "Failed", "Dead", "Superseded"}
TerminalSource == {"None", "Natural", "CascadeCancel"}
RPCKind == {"Cancel", "Ack"}

ASSUME DeploymentIsFiniteSet == IsFiniteSet(Deployment)
ASSUME ParentDeploymentInDeployment == ParentDeployment \in Deployment
ASSUME ChildDeploymentInDeployment == ChildDeployment \in Deployment
ASSUME ParentChildDeploymentDistinct == ParentDeployment # ChildDeployment
ASSUME ChildIsFiniteSet == IsFiniteSet(Child)
ASSUME RPCIdIsFiniteSet == IsFiniteSet(RPCId)
ASSUME MaxCrashesIsNat == MaxCrashes \in Nat
ASSUME MaxDropsIsNat == MaxDrops \in Nat
ASSUME NoOfNotInRPCId == NoOf \notin RPCId

RPC == [
  id    : RPCId,
  kind  : RPCKind,
  src   : Deployment,
  tgt   : Deployment,
  child : Child,
  of    : RPCId \cup {NoOf}
]

VARIABLES
  cancelIntentA,
  cancelAckedA,
  cancelAttemptCountA,
  childStateB,
  terminalSourceB,
  terminalWriteCountB,
  cancelHandledB,
  cancelHandleCountB,
  inFlight,
  messages,
  pendingInbound,
  rpcIdsUsed,
  dropCount,
  crashCount

vars == <<
  cancelIntentA,
  cancelAckedA,
  cancelAttemptCountA,
  childStateB,
  terminalSourceB,
  terminalWriteCountB,
  cancelHandledB,
  cancelHandleCountB,
  inFlight,
  messages,
  pendingInbound,
  rpcIdsUsed,
  dropCount,
  crashCount
>>

TypeOK ==
  /\ cancelIntentA       \in [Child -> BOOLEAN]
  /\ cancelAckedA        \in [Child -> BOOLEAN]
  /\ cancelAttemptCountA \in [Child -> 0..Cardinality(RPCId)]
  /\ childStateB         \in [Child -> ChildState]
  /\ terminalSourceB     \in [Child -> TerminalSource]
  /\ terminalWriteCountB \in [Child -> 0..2]
  /\ cancelHandledB      \in [Child -> BOOLEAN]
  /\ cancelHandleCountB  \in [Child -> 0..2]
  /\ inFlight            \in [Deployment -> SUBSET RPC]
  /\ messages            \in SUBSET RPC
  /\ pendingInbound      \in [Deployment -> SUBSET RPC]
  /\ rpcIdsUsed          \in SUBSET RPCId
  /\ dropCount           \in 0..MaxDrops
  /\ crashCount          \in [Deployment -> 0..MaxCrashes]

Init ==
  /\ cancelIntentA       = [child \in Child |-> FALSE]
  /\ cancelAckedA        = [child \in Child |-> FALSE]
  /\ cancelAttemptCountA = [child \in Child |-> 0]
  /\ childStateB         = [child \in Child |-> "Running"]
  /\ terminalSourceB     = [child \in Child |-> "None"]
  /\ terminalWriteCountB = [child \in Child |-> 0]
  /\ cancelHandledB      = [child \in Child |-> FALSE]
  /\ cancelHandleCountB  = [child \in Child |-> 0]
  /\ inFlight            = [deployment \in Deployment |-> {}]
  /\ messages            = {}
  /\ pendingInbound      = [deployment \in Deployment |-> {}]
  /\ rpcIdsUsed          = {}
  /\ dropCount           = 0
  /\ crashCount          = [deployment \in Deployment |-> 0]

(***************************************************************************)
(* Helpers.                                                                *)
(***************************************************************************)

AllRPCs ==
  messages
    \cup UNION { inFlight[deployment] : deployment \in Deployment }
    \cup UNION { pendingInbound[deployment] : deployment \in Deployment }

FreshIds(k) ==
  Cardinality(RPCId \ rpcIdsUsed) >= k

PendingCancelFor(child) ==
  \E rpc \in inFlight[ParentDeployment] :
    /\ rpc.kind = "Cancel"
    /\ rpc.child = child

(***************************************************************************)
(* A-side durable cascade intent and retry emission.                       *)
(***************************************************************************)

InvokeBridgeCancelCascade(child) ==
  /\ child \in Child
  /\ childStateB[child] = "Running"
  /\ cancelIntentA[child] = FALSE
  /\ cancelIntentA' = [cancelIntentA EXCEPT ![child] = TRUE]
  /\ UNCHANGED <<
       cancelAckedA,
       cancelAttemptCountA,
       childStateB,
       terminalSourceB,
       terminalWriteCountB,
       cancelHandledB,
       cancelHandleCountB,
       inFlight,
       messages,
       pendingInbound,
       rpcIdsUsed,
       dropCount,
       crashCount
     >>

EmitCancel(child) ==
  /\ child \in Child
  /\ cancelIntentA[child]
  /\ ~PendingCancelFor(child)
  /\ FreshIds(1)
  /\ LET id == CHOOSE rpcId \in RPCId \ rpcIdsUsed : TRUE
         rpc == [
           id    |-> id,
           kind  |-> "Cancel",
           src   |-> ParentDeployment,
           tgt   |-> ChildDeployment,
           child |-> child,
           of    |-> NoOf
         ]
     IN /\ inFlight' =
             [inFlight EXCEPT ![ParentDeployment] = @ \cup {rpc}]
        /\ messages' = messages \cup {rpc}
        /\ rpcIdsUsed' = rpcIdsUsed \cup {id}
  /\ cancelAttemptCountA' =
       [cancelAttemptCountA EXCEPT ![child] = @ + 1]
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAckedA,
       childStateB,
       terminalSourceB,
       terminalWriteCountB,
       cancelHandledB,
       cancelHandleCountB,
       pendingInbound,
       dropCount,
       crashCount
     >>

(***************************************************************************)
(* Safety invariants.                                                      *)
(***************************************************************************)

RPCIdsTracked ==
  \A rpc \in AllRPCs : rpc.id \in rpcIdsUsed

RPCWellFormed ==
  \A rpc \in AllRPCs :
    /\ rpc.kind \in RPCKind
    /\ \/ /\ rpc.kind = "Cancel"
          /\ rpc.src = ParentDeployment
          /\ rpc.tgt = ChildDeployment
          /\ rpc.of = NoOf
       \/ /\ rpc.kind = "Ack"
          /\ rpc.src = ChildDeployment
          /\ rpc.tgt = ParentDeployment
          /\ rpc.of \in rpcIdsUsed

CancelIntentCausal ==
  \A child \in Child :
    (cancelAttemptCountA[child] > 0
      \/ \E rpc \in AllRPCs :
           /\ rpc.child = child
           /\ rpc.kind \in {"Cancel", "Ack"})
      => cancelIntentA[child]

Next ==
  \/ \E child \in Child : InvokeBridgeCancelCascade(child)
  \/ \E child \in Child : EmitCancel(child)

Spec == Init /\ [][Next]_vars

====
