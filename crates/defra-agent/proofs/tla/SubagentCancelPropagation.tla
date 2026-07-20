---- MODULE SubagentCancelPropagation ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Cross-deployment subagent cascade-cancel propagation.                   *)
(*                                                                         *)
(* Spec design:                                                            *)
(*   docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-        *)
(*   tla-design.md (removed from the tree; see git history)                *)
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
(* Cross-deployment channel interleavings.                                 *)
(***************************************************************************)

Deliver(rpc) ==
  /\ rpc \in messages
  /\ messages' = messages \ {rpc}
  /\ pendingInbound' =
       [pendingInbound EXCEPT ![rpc.tgt] = @ \cup {rpc}]
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAckedA,
       cancelAttemptCountA,
       childStateB,
       terminalSourceB,
       terminalWriteCountB,
       cancelHandledB,
       cancelHandleCountB,
       inFlight,
       rpcIdsUsed,
       dropCount,
       crashCount
     >>

Drop(rpc) ==
  /\ rpc \in messages
  /\ dropCount < MaxDrops
  /\ messages' = messages \ {rpc}
  /\ dropCount' = dropCount + 1
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAckedA,
       cancelAttemptCountA,
       childStateB,
       terminalSourceB,
       terminalWriteCountB,
       cancelHandledB,
       cancelHandleCountB,
       inFlight,
       pendingInbound,
       rpcIdsUsed,
       crashCount
     >>

Timeout(rpc) ==
  /\ rpc \in inFlight[ParentDeployment]
  /\ rpc.kind = "Cancel"
  /\ inFlight' =
       [inFlight EXCEPT ![ParentDeployment] = @ \ {rpc}]
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAckedA,
       cancelAttemptCountA,
       childStateB,
       terminalSourceB,
       terminalWriteCountB,
       cancelHandledB,
       cancelHandleCountB,
       messages,
       pendingInbound,
       rpcIdsUsed,
       dropCount,
       crashCount
     >>

Crash(deployment) ==
  /\ deployment \in Deployment
  /\ crashCount[deployment] < MaxCrashes
  /\ inFlight' = [inFlight EXCEPT ![deployment] = {}]
  /\ pendingInbound' = [pendingInbound EXCEPT ![deployment] = {}]
  /\ crashCount' = [crashCount EXCEPT ![deployment] = @ + 1]
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAckedA,
       cancelAttemptCountA,
       childStateB,
       terminalSourceB,
       terminalWriteCountB,
       cancelHandledB,
       cancelHandleCountB,
       messages,
       rpcIdsUsed,
       dropCount
     >>

(***************************************************************************)
(* B-side natural child terminal race.                                     *)
(***************************************************************************)

NaturalTerminalize(child, terminal) ==
  /\ child \in Child
  /\ terminal \in NaturalTerminal
  /\ childStateB[child] = "Running"
  /\ childStateB' = [childStateB EXCEPT ![child] = terminal]
  /\ terminalSourceB' = [terminalSourceB EXCEPT ![child] = "Natural"]
  /\ terminalWriteCountB' =
       [terminalWriteCountB EXCEPT ![child] = @ + 1]
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAckedA,
       cancelAttemptCountA,
       cancelHandledB,
       cancelHandleCountB,
       inFlight,
       messages,
       pendingInbound,
       rpcIdsUsed,
       dropCount,
       crashCount
     >>

(***************************************************************************)
(* B-side cancel handling and A-side ack receipt.                          *)
(***************************************************************************)

ProcessCancel(rpc) ==
  /\ rpc \in pendingInbound[ChildDeployment]
  /\ rpc.kind = "Cancel"
  /\ rpc.tgt = ChildDeployment
  /\ FreshIds(1)
  /\ LET child == rpc.child
         firstHandle == ~cancelHandledB[child]
         liveChild == childStateB[child] = "Running"
         ackId == CHOOSE rpcId \in RPCId \ rpcIdsUsed : TRUE
         ack == [
           id    |-> ackId,
           kind  |-> "Ack",
           src   |-> ChildDeployment,
           tgt   |-> ParentDeployment,
           child |-> child,
           of    |-> rpc.id
         ]
     IN /\ pendingInbound' =
             [pendingInbound EXCEPT ![ChildDeployment] = @ \ {rpc}]
        /\ cancelHandledB' =
             IF firstHandle
             THEN [cancelHandledB EXCEPT ![child] = TRUE]
             ELSE cancelHandledB
        /\ cancelHandleCountB' =
             IF firstHandle
             THEN [cancelHandleCountB EXCEPT ![child] = @ + 1]
             ELSE cancelHandleCountB
        /\ childStateB' =
             IF firstHandle /\ liveChild
             THEN [childStateB EXCEPT ![child] = "Interrupted"]
             ELSE childStateB
        /\ terminalSourceB' =
             IF firstHandle /\ liveChild
             THEN [terminalSourceB EXCEPT ![child] = "CascadeCancel"]
             ELSE terminalSourceB
        /\ terminalWriteCountB' =
             IF firstHandle /\ liveChild
             THEN [terminalWriteCountB EXCEPT ![child] = @ + 1]
             ELSE terminalWriteCountB
        /\ messages' = messages \cup {ack}
        /\ rpcIdsUsed' = rpcIdsUsed \cup {ackId}
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAckedA,
       cancelAttemptCountA,
       inFlight,
       dropCount,
       crashCount
     >>

ReceiveAck(ack) ==
  /\ ack \in pendingInbound[ParentDeployment]
  /\ ack.kind = "Ack"
  /\ ack.tgt = ParentDeployment
  /\ \E rpc \in inFlight[ParentDeployment] :
       /\ rpc.kind = "Cancel"
       /\ rpc.id = ack.of
       /\ rpc.child = ack.child
  /\ pendingInbound' =
       [pendingInbound EXCEPT ![ParentDeployment] = @ \ {ack}]
  /\ inFlight' =
       [inFlight EXCEPT ![ParentDeployment] =
         { rpc \in @ : rpc.id # ack.of }]
  /\ cancelAckedA' =
       [cancelAckedA EXCEPT ![ack.child] = TRUE]
  /\ UNCHANGED <<
       cancelIntentA,
       cancelAttemptCountA,
       childStateB,
       terminalSourceB,
       terminalWriteCountB,
       cancelHandledB,
       cancelHandleCountB,
       messages,
       rpcIdsUsed,
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
    (cancelHandledB[child]
      \/ cancelAckedA[child]
      \/ cancelAttemptCountA[child] > 0
      \/ \E rpc \in AllRPCs :
           /\ rpc.child = child
           /\ rpc.kind \in {"Cancel", "Ack"})
      => cancelIntentA[child]

AckRequiresHandled ==
  /\ \A child \in Child :
       cancelAckedA[child] => cancelHandledB[child]
  /\ \A rpc \in AllRPCs :
       rpc.kind = "Ack" => cancelHandledB[rpc.child]

CancelHandledIdempotent ==
  \A child \in Child : cancelHandleCountB[child] <= 1

CascadeInterruptsOnlyRunning ==
  \A child \in Child :
    terminalSourceB[child] = "CascadeCancel" =>
      /\ childStateB[child] = "Interrupted"
      /\ cancelHandledB[child]

InterruptedOnlyByCascade ==
  \A child \in Child :
    childStateB[child] = "Interrupted" =>
      terminalSourceB[child] = "CascadeCancel"

InterruptExactlyOnce ==
  \A child \in Child : terminalWriteCountB[child] <= 1

NaturalTerminalStableAfterCancel ==
  \A child \in Child :
    terminalSourceB[child] = "Natural" =>
      /\ childStateB[child] \in NaturalTerminal
      /\ terminalWriteCountB[child] = 1

HandledCancelStable ==
  \A child \in Child :
    cancelHandledB[child] =>
      \/ /\ terminalSourceB[child] = "CascadeCancel"
         /\ childStateB[child] = "Interrupted"
      \/ /\ terminalSourceB[child] = "Natural"
         /\ childStateB[child] \in NaturalTerminal

(***************************************************************************)
(* State constraint for bounded liveness checking.                         *)
(*                                                                         *)
(* The real system has effectively unbounded RPC ids. TLC does not. This  *)
(* constraint excludes only states where the finite id pool has been fully *)
(* consumed before an unhandled child cancel can allocate the ack id used  *)
(* by ProcessCancel. Once B has durably handled the cancel, the main #188  *)
(* liveness target is satisfied even if later ack retirement runs out of   *)
(* bounded ids.                                                            *)
(***************************************************************************)

StateBound ==
  \A child \in Child :
    ~cancelHandledB[child] => FreshIds(1)

Next ==
  \/ \E child \in Child : InvokeBridgeCancelCascade(child)
  \/ \E child \in Child : EmitCancel(child)
  \/ \E rpc \in messages : Deliver(rpc)
  \/ \E rpc \in messages : Drop(rpc)
  \/ \E rpc \in pendingInbound[ChildDeployment] : ProcessCancel(rpc)
  \/ \E ack \in pendingInbound[ParentDeployment] : ReceiveAck(ack)
  \/ \E child \in Child, terminal \in NaturalTerminal :
       NaturalTerminalize(child, terminal)
  \/ \E rpc \in inFlight[ParentDeployment] : Timeout(rpc)
  \/ \E deployment \in Deployment : Crash(deployment)

(***************************************************************************)
(* Fairness and liveness.                                                  *)
(***************************************************************************)

Fairness ==
  /\ \A child \in Child : WF_vars(EmitCancel(child))
  /\ WF_vars(\E rpc \in messages : Deliver(rpc))
  /\ WF_vars(\E rpc \in pendingInbound[ChildDeployment] : ProcessCancel(rpc))
  /\ WF_vars(\E ack \in pendingInbound[ParentDeployment] : ReceiveAck(ack))
  /\ WF_vars(\E rpc \in inFlight[ParentDeployment] : Timeout(rpc))

Spec == Init /\ [][Next]_vars /\ Fairness

CancelDeliveryProgress ==
  \A child \in Child :
    cancelIntentA[child] ~> cancelHandledB[child]

LiveCancelInterruptsOrNaturalWins ==
  \A child \in Child :
    /\ cancelIntentA[child]
    /\ childStateB[child] = "Running"
    ~> \/ childStateB[child] = "Interrupted"
       \/ terminalSourceB[child] = "Natural"

(***************************************************************************)
(* Documented but not enforced by the default TLC config.                  *)
(*                                                                         *)
(* Ack progress can fail in bounded-pool-exhausted states after B has      *)
(* already durably handled the cancel but A has crashed or timed out away  *)
(* the matching in-flight attempt. That is an RPCId-pool artifact, not the *)
(* #188 delivery property. The enforced safety invariant remains           *)
(* AckRequiresHandled: every ack that exists is backed by B durable         *)
(* handling.                                                               *)
(***************************************************************************)

CancelAckProgress ==
  \A child \in Child :
    cancelHandledB[child] ~> cancelAckedA[child]

CancelPropagationProgress ==
  /\ CancelDeliveryProgress
  /\ LiveCancelInterruptsOrNaturalWins

====
