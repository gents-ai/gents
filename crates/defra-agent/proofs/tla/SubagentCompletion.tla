---- MODULE SubagentCompletion ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Cross-deployment background subagent completion projection.              *)
(*                                                                         *)
(* Spec design:                                                            *)
(*   docs/superpowers/specs/2026-05-12-subagent-completion-cross-          *)
(*   deployment-tla-design.md                                              *)
(*                                                                         *)
(* Parent bridge rows live on deployment A; child request terminal state   *)
(* is durable on deployment B; A learns through document-gossip delivery.  *)
(***************************************************************************)

CONSTANTS
  Deployment,
  Child,
  EventId,
  QueueId,
  MaxCrashes,
  MaxDrops,
  NoTerminal,
  ParentSession,
  CompletionQueueKey,
  UserQueueKey

TerminalKind == {"Completed", "Failed", "Dead", "Interrupted", "Superseded"}
BridgeState == {"Running", "Completed", "Failed", "Cancelled"}
TerminalSource == {"None", "ChildProjection", "ParentCancel"}
QueueSource == {"user", "subagent_completion"}
QueuePolicy == {"append", "coalesce"}
QueueState == {"pending", "drained"}
QueueKey == {CompletionQueueKey, UserQueueKey}

ASSUME DeploymentIsFiniteSet == IsFiniteSet(Deployment)
ASSUME ChildIsFiniteSet == IsFiniteSet(Child)
ASSUME EventIdIsFiniteSet == IsFiniteSet(EventId)
ASSUME QueueIdIsFiniteSet == IsFiniteSet(QueueId)
ASSUME MaxCrashesIsNat == MaxCrashes \in Nat
ASSUME MaxDropsIsNat == MaxDrops \in Nat
ASSUME NoTerminalNotTerminal == NoTerminal \notin TerminalKind
ASSUME CompletionQueueKeyNotUserQueueKey == CompletionQueueKey # UserQueueKey

Observation == [
  id       : EventId,
  child    : Child,
  terminal : TerminalKind
]

QueueRow == [
  id      : QueueId,
  session : {ParentSession},
  source  : QueueSource,
  policy  : QueuePolicy,
  key     : QueueKey,
  state   : QueueState
]

VARIABLES
  childDurable,
  childFinalResponseDurable,
  messages,
  pendingInboundA,
  observedDurableA,
  bridgeState,
  terminalSource,
  terminalWriteCount,
  notificationDurable,
  queueRows,
  queueIdsUsed,
  eventIdsUsed,
  dropCount,
  crashCount,
  cancelRequested

vars == <<
  childDurable,
  childFinalResponseDurable,
  messages,
  pendingInboundA,
  observedDurableA,
  bridgeState,
  terminalSource,
  terminalWriteCount,
  notificationDurable,
  queueRows,
  queueIdsUsed,
  eventIdsUsed,
  dropCount,
  crashCount,
  cancelRequested
>>

TypeOK ==
  /\ childDurable              \in [Child -> TerminalKind \cup {NoTerminal}]
  /\ childFinalResponseDurable \in [Child -> BOOLEAN]
  /\ messages                  \in SUBSET Observation
  /\ pendingInboundA           \in SUBSET Observation
  /\ observedDurableA          \in [Child -> TerminalKind \cup {NoTerminal}]
  /\ bridgeState               \in [Child -> BridgeState]
  /\ terminalSource            \in [Child -> TerminalSource]
  /\ terminalWriteCount        \in [Child -> 0..2]
  /\ notificationDurable       \in [Child -> BOOLEAN]
  /\ queueRows                 \in SUBSET QueueRow
  /\ queueIdsUsed              \in SUBSET QueueId
  /\ eventIdsUsed              \in SUBSET EventId
  /\ dropCount                 \in 0..MaxDrops
  /\ crashCount                \in [Deployment -> 0..MaxCrashes]
  /\ cancelRequested           \in [Child -> BOOLEAN]

Init ==
  /\ childDurable              = [child \in Child |-> NoTerminal]
  /\ childFinalResponseDurable = [child \in Child |-> FALSE]
  /\ messages                  = {}
  /\ pendingInboundA           = {}
  /\ observedDurableA          = [child \in Child |-> NoTerminal]
  /\ bridgeState               = [child \in Child |-> "Running"]
  /\ terminalSource            = [child \in Child |-> "None"]
  /\ terminalWriteCount        = [child \in Child |-> 0]
  /\ notificationDurable       = [child \in Child |-> FALSE]
  /\ queueRows                 = {}
  /\ queueIdsUsed              = {}
  /\ eventIdsUsed              = {}
  /\ dropCount                 = 0
  /\ crashCount                = [deployment \in Deployment |-> 0]
  /\ cancelRequested           = [child \in Child |-> FALSE]

(***************************************************************************)
(* B-side durable child terminal writes.                                   *)
(***************************************************************************)

PersistChildTerminal(child, terminal) ==
  /\ child \in Child
  /\ terminal \in TerminalKind
  /\ childDurable[child] = NoTerminal
  /\ childDurable' =
       [childDurable EXCEPT ![child] = terminal]
  /\ childFinalResponseDurable' =
       [childFinalResponseDurable EXCEPT ![child] = TRUE]
  /\ UNCHANGED <<
       messages,
       pendingInboundA,
       observedDurableA,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       dropCount,
       crashCount,
       cancelRequested
     >>

(***************************************************************************)
(* Document-gossip delivery from B to A.                                   *)
(***************************************************************************)

FreshEventIds(k) ==
  Cardinality(EventId \ eventIdsUsed) >= k

AllObservations == messages \cup pendingInboundA

EmitTerminalObservation(child) ==
  /\ child \in Child
  /\ childDurable[child] # NoTerminal
  /\ FreshEventIds(1)
  /\ LET id == CHOOSE eventId \in EventId \ eventIdsUsed : TRUE
         obs == [
           id       |-> id,
           child    |-> child,
           terminal |-> childDurable[child]
         ]
     IN /\ messages' = messages \cup {obs}
        /\ eventIdsUsed' = eventIdsUsed \cup {id}
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       pendingInboundA,
       observedDurableA,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       dropCount,
       crashCount,
       cancelRequested
     >>

DeliverObservation(obs) ==
  /\ obs \in messages
  /\ messages' = messages \ {obs}
  /\ pendingInboundA' = pendingInboundA \cup {obs}
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       observedDurableA,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       dropCount,
       crashCount,
       cancelRequested
     >>

DropObservation(obs) ==
  /\ obs \in messages
  /\ dropCount < MaxDrops
  /\ messages' = messages \ {obs}
  /\ dropCount' = dropCount + 1
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       pendingInboundA,
       observedDurableA,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       crashCount,
       cancelRequested
     >>

PersistObservationOnA(obs) ==
  /\ obs \in pendingInboundA
  /\ observedDurableA[obs.child] \in {NoTerminal, obs.terminal}
  /\ pendingInboundA' = pendingInboundA \ {obs}
  /\ observedDurableA' =
       [observedDurableA EXCEPT ![obs.child] = obs.terminal]
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       messages,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       dropCount,
       crashCount,
       cancelRequested
     >>

(***************************************************************************)
(* Safety invariants.                                                      *)
(***************************************************************************)

DurableChildTerminalOK ==
  \A child \in Child :
    childDurable[child] # NoTerminal => childFinalResponseDurable[child]

EventIdsTracked ==
  \A obs \in AllObservations : obs.id \in eventIdsUsed

ObservationBackedByBDurable ==
  \A obs \in AllObservations :
    /\ childDurable[obs.child] = obs.terminal
    /\ childFinalResponseDurable[obs.child]

ADurableObservationBackedByB ==
  \A child \in Child :
    observedDurableA[child] # NoTerminal =>
      /\ childDurable[child] = observedDurableA[child]
      /\ childFinalResponseDurable[child]

StateBound == TRUE

Next ==
  \/ \E child \in Child, terminal \in TerminalKind :
       PersistChildTerminal(child, terminal)
  \/ \E child \in Child : EmitTerminalObservation(child)
  \/ \E obs \in messages : DeliverObservation(obs)
  \/ \E obs \in messages : DropObservation(obs)
  \/ \E obs \in pendingInboundA : PersistObservationOnA(obs)

Spec == Init /\ [][Next]_vars

====
