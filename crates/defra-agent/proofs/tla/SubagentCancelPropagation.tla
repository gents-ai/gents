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

Next == FALSE

Spec == Init /\ [][Next]_vars

====
