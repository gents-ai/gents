---- MODULE SubagentCompletion ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Cross-deployment background subagent completion projection.              *)
(*                                                                         *)
(* Parent bridge rows live on deployment A; child request terminal state   *)
(* is durable on deployment B; A learns through document-gossip delivery.  *)
(***************************************************************************)

CONSTANTS
  Deployment,
  ParentDeployment,
  ChildDeployment,
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
ASSUME ParentDeploymentInDeployment == ParentDeployment \in Deployment
ASSUME ChildDeploymentInDeployment == ChildDeployment \in Deployment
ASSUME ParentChildDeploymentDistinct == ParentDeployment # ChildDeployment
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
(* A-side parent bridge projection.                                        *)
(***************************************************************************)

ProjectedBridgeState(terminal) ==
  IF terminal = "Completed" THEN "Completed" ELSE "Failed"

ProjectTerminal(child) ==
  /\ child \in Child
  /\ bridgeState[child] = "Running"
  /\ terminalSource[child] = "None"
  /\ observedDurableA[child] # NoTerminal
  /\ bridgeState' =
       [bridgeState EXCEPT ![child] = ProjectedBridgeState(observedDurableA[child])]
  /\ terminalSource' =
       [terminalSource EXCEPT ![child] = "ChildProjection"]
  /\ terminalWriteCount' =
       [terminalWriteCount EXCEPT ![child] = @ + 1]
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       messages,
       pendingInboundA,
       observedDurableA,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       dropCount,
       crashCount,
       cancelRequested
     >>

(***************************************************************************)
(* Durable transcript notification append on A.                            *)
(***************************************************************************)

AppendNotification(child) ==
  /\ child \in Child
  /\ terminalSource[child] = "ChildProjection"
  /\ notificationDurable[child] = FALSE
  /\ notificationDurable' =
       [notificationDurable EXCEPT ![child] = TRUE]
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       messages,
       pendingInboundA,
       observedDurableA,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       dropCount,
       crashCount,
       cancelRequested
     >>

(***************************************************************************)
(* Parent-session request queue. Background completion never writes here.  *)
(***************************************************************************)

FreshQueueIds(k) ==
  Cardinality(QueueId \ queueIdsUsed) >= k

IsPendingCompletionWakeup(row) ==
  /\ row.session = ParentSession
  /\ row.source = "subagent_completion"
  /\ row.policy = "coalesce"
  /\ row.key = CompletionQueueKey
  /\ row.state = "pending"

IsCompletionWakeup(row) ==
  /\ row.session = ParentSession
  /\ row.source = "subagent_completion"
  /\ row.policy = "coalesce"
  /\ row.key = CompletionQueueKey

HasPendingCompletionWakeup ==
  \E row \in queueRows : IsPendingCompletionWakeup(row)

HasCompletionWakeup ==
  \E row \in queueRows : IsCompletionWakeup(row)

IsUserRequest(row) ==
  /\ row.session = ParentSession
  /\ row.source = "user"
  /\ row.policy = "append"
  /\ row.key = UserQueueKey

HasUserRequest ==
  \E row \in queueRows : IsUserRequest(row)

EnqueueUserRequest ==
  /\ ~HasUserRequest
  /\ FreshQueueIds(1)
  /\ LET id == CHOOSE queueId \in QueueId \ queueIdsUsed : TRUE
         row == [
           id      |-> id,
           session |-> ParentSession,
           source  |-> "user",
           policy  |-> "append",
           key     |-> UserQueueKey,
           state   |-> "pending"
         ]
     IN /\ queueRows' = queueRows \cup {row}
        /\ queueIdsUsed' = queueIdsUsed \cup {id}
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       messages,
       pendingInboundA,
       observedDurableA,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       notificationDurable,
       eventIdsUsed,
       dropCount,
       crashCount,
       cancelRequested
     >>

(***************************************************************************)
(* Parent cancellation winning a completion race.                          *)
(***************************************************************************)

CancelParent(child) ==
  /\ child \in Child
  /\ bridgeState[child] = "Running"
  /\ terminalSource[child] = "None"
  /\ bridgeState' = [bridgeState EXCEPT ![child] = "Cancelled"]
  /\ terminalSource' = [terminalSource EXCEPT ![child] = "ParentCancel"]
  /\ terminalWriteCount' = [terminalWriteCount EXCEPT ![child] = @ + 1]
  /\ cancelRequested' = [cancelRequested EXCEPT ![child] = TRUE]
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       messages,
       pendingInboundA,
       observedDurableA,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       dropCount,
       crashCount
     >>

(***************************************************************************)
(* Crash/recovery abstraction.                                             *)
(***************************************************************************)

CrashA ==
  /\ crashCount[ParentDeployment] < MaxCrashes
  /\ pendingInboundA' = {}
  /\ crashCount' =
       [crashCount EXCEPT ![ParentDeployment] = @ + 1]
  /\ UNCHANGED <<
       childDurable,
       childFinalResponseDurable,
       messages,
       observedDurableA,
       bridgeState,
       terminalSource,
       terminalWriteCount,
       notificationDurable,
       queueRows,
       queueIdsUsed,
       eventIdsUsed,
       dropCount,
       cancelRequested
     >>

CrashB ==
  /\ crashCount[ChildDeployment] < MaxCrashes
  /\ crashCount' =
       [crashCount EXCEPT ![ChildDeployment] = @ + 1]
  /\ UNCHANGED <<
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

BridgeTerminalUnique ==
  \A child \in Child : terminalWriteCount[child] <= 1

ProjectionRequiresBDurableTerminal ==
  \A child \in Child :
    terminalSource[child] = "ChildProjection" =>
      /\ childDurable[child] = observedDurableA[child]
      /\ childFinalResponseDurable[child]

ProjectionRequiresADurableObservation ==
  \A child \in Child :
    terminalSource[child] = "ChildProjection" =>
      observedDurableA[child] # NoTerminal

ProjectionMatchesLeanBridgeMapping ==
  \A child \in Child :
    terminalSource[child] = "ChildProjection" =>
      bridgeState[child] = ProjectedBridgeState(observedDurableA[child])

CancelledOnlyByParentCancel ==
  \A child \in Child :
    bridgeState[child] = "Cancelled" => terminalSource[child] = "ParentCancel"

NotificationCausal ==
  \A child \in Child :
    notificationDurable[child] => terminalSource[child] = "ChildProjection"

QueueIdsTracked ==
  \A row \in queueRows : row.id \in queueIdsUsed

NoSyntheticCompletionWakeups ==
  ~HasCompletionWakeup

UserPendingPreserved ==
  \A row \in queueRows :
    row.source = "user" => row.state = "pending"

ParentCancelAbsorbsLateTerminal ==
  \A child \in Child :
    terminalSource[child] = "ParentCancel" =>
      /\ bridgeState[child] = "Cancelled"
      /\ terminalWriteCount[child] = 1
      /\ notificationDurable[child] = FALSE

CancelRequestedCausal ==
  \A child \in Child :
    cancelRequested[child] => terminalSource[child] = "ParentCancel"

StateBound ==
  /\ Cardinality(eventIdsUsed) <= 3
  /\ Cardinality(queueIdsUsed) <= 2

Next ==
  \/ \E child \in Child, terminal \in TerminalKind :
       PersistChildTerminal(child, terminal)
  \/ \E child \in Child : EmitTerminalObservation(child)
  \/ \E obs \in messages : DeliverObservation(obs)
  \/ \E obs \in messages : DropObservation(obs)
  \/ \E obs \in pendingInboundA : PersistObservationOnA(obs)
  \/ \E child \in Child : ProjectTerminal(child)
  \/ \E child \in Child : AppendNotification(child)
  \/ EnqueueUserRequest
  \/ \E child \in Child : CancelParent(child)
  \/ CrashA
  \/ CrashB

(***************************************************************************)
(* Fairness and liveness.                                                  *)
(***************************************************************************)

Fairness ==
  /\ \A child \in Child : WF_vars(EmitTerminalObservation(child))
  /\ WF_vars(\E obs \in messages : DeliverObservation(obs))
  /\ WF_vars(\E obs \in pendingInboundA : PersistObservationOnA(obs))
  /\ \A child \in Child : WF_vars(ProjectTerminal(child))
  /\ \A child \in Child : WF_vars(AppendNotification(child))

Spec == Init /\ [][Next]_vars /\ Fairness

DurableTerminalSettles ==
  \A child \in Child :
    /\ childDurable[child] # NoTerminal
    /\ terminalSource[child] = "None"
    ~> \/ terminalSource[child] = "ChildProjection"
       \/ terminalSource[child] = "ParentCancel"

LiveBridgeTerminalProjects ==
  \A child \in Child :
    /\ childDurable[child] # NoTerminal
    /\ bridgeState[child] = "Running"
    /\ terminalSource[child] = "None"
    ~> \/ terminalSource[child] = "ChildProjection"
       \/ terminalSource[child] = "ParentCancel"

ProjectionNotifies ==
  \A child \in Child :
    terminalSource[child] = "ChildProjection"
      ~> notificationDurable[child]

CompletionProgress ==
  /\ DurableTerminalSettles
  /\ LiveBridgeTerminalProjects
  /\ ProjectionNotifies

====
