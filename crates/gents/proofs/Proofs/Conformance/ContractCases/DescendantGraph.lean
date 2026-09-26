import Proofs.DescendantGraph

namespace Conformance.ContractCases

open DescendantGraph

structure DescendantGraphCase where
  name : String
  rootRequestId : Nat
  parentRequestId : Nat
  childRequestId : Nat
  awaitMode : String
  materialization : String
  lifecycle : String
  direct : Bool
  visible : Bool
  readable : Bool
  retryable : Bool
  listedByDefault : Bool
  controllable : Bool
  cursorAnchorSurvivesTerminal : Bool
  callerSession : String
  callerAgent : String
  callerRequester : Option String
  sessionAuthorized : Bool
  sessionControllable : Bool
  childState : Option String
  spawnUnclaimed : Bool
  cancelIntent : Bool
  steerAdmission : String
  deriving Repr

def viewer : Viewer :=
  { rootRequestId := 1
  , rootPrincipal := 10
  , rootSessionId := 100
  , lineageId := 1000 }

def baseEdge : Edge :=
  { rootRequestId := 1
  , rootSessionId := 100
  , parentRequestId := 1
  , parentToolCallId := 20
  , childRequestId := 2
  , childSessionId := some 200
  , ownerPrincipal := 10
  , controlPrincipal := 10
  , childPrincipal := 11
  , behaviorId := 30
  , lineageId := 1000
  , awaitMode := .background
  , materialization := .local
  , lifecycle := .running
  , bridgeDurable := true
  , physicalCorroborated := true
  , directFromRoot := true }

def awaitModeString : AwaitMode → String
  | .foreground => "foreground"
  | .background => "background"

def materializationString : Materialization → String
  | .pending => "pending"
  | .local => "local"
  | .replicated => "replicated"

def lifecycleString : Lifecycle → String
  | .pending => "pending"
  | .running => "running"
  | .completed => "completed"
  | .failed => "failed"
  | .timedOut => "timedOut"
  | .cancelled => "cancelled"

def steerAdmissionString : SteerAdmission → String
  | .notAuthorized => "not_authorized"
  | .notBackgrounded => "not_backgrounded"
  | .cancelled => "cancelled"
  | .terminal => "terminal"
  | .awaitingMaterialization => "awaiting_materialization"
  | .fenced => "fenced"
  | .append => "append"

/-- Evidence beside an unfenced child whose request is in `child`. -/
def childEvidence (child : RequestState) : SteerEvidence :=
  { child := some child, spawnUnclaimed := false, cancelIntent := false }

def liveEvidence : SteerEvidence := childEvidence .processing

def sessionOwner : SessionOwner :=
  { sessionId := "conversation", agentDid := "did:owner", requesterDid := none }

def descendantCase (name : String) (edge : Edge)
    (caller : SessionOwner := sessionOwner)
    (evidence : SteerEvidence := liveEvidence) : DescendantGraphCase :=
  { name
  , rootRequestId := edge.rootRequestId
  , parentRequestId := edge.parentRequestId
  , childRequestId := edge.childRequestId
  , awaitMode := awaitModeString edge.awaitMode
  , materialization := materializationString edge.materialization
  , lifecycle := lifecycleString edge.lifecycle
  , direct := edge.directFromRoot
  , visible := DescendantGraph.visible viewer edge
  , readable := DescendantGraph.readable viewer edge
  , retryable := DescendantGraph.retryable viewer edge
  , listedByDefault := DescendantGraph.listedByDefault viewer edge
  , controllable := DescendantGraph.controllable viewer edge
  , cursorAnchorSurvivesTerminal :=
      (DescendantGraph.afterCursor
        (DescendantGraph.cursor edge)
        [{ edge with lifecycle := .completed }, baseEdge]).isSome
  , callerSession := caller.sessionId
  , callerAgent := caller.agentDid
  , callerRequester := caller.requesterDid
  , sessionAuthorized := sameSessionOwner caller sessionOwner
  , sessionControllable := DescendantGraph.sessionControllable caller sessionOwner viewer edge
  , childState := evidence.child.map RequestState.toDefraDB
  , spawnUnclaimed := evidence.spawnUnclaimed
  , cancelIntent := evidence.cancelIntent
  , steerAdmission := steerAdmissionString (DescendantGraph.steerAdmission viewer edge evidence) }

def descendantGraphCases : List DescendantGraphCase :=
  [ descendantCase "background_direct" baseEdge
  , descendantCase "foreground_direct"
      { { baseEdge with awaitMode := .foreground } with childRequestId := 3 }
  , descendantCase "nested_visible_not_controllable"
      { { { { { baseEdge with parentRequestId := 5 } with
          parentToolCallId := 21 } with childRequestId := 6 } with
          directFromRoot := false } with controlPrincipal := 11 }
  , descendantCase "unmaterialized_remote_bridge"
      { baseEdge with
          childRequestId := 7, childSessionId := none,
          materialization := .pending, physicalCorroborated := false }
  , descendantCase "terminal_unmaterialized_remote_bridge"
      { { { { { baseEdge with childRequestId := 14 } with childSessionId := none } with
          materialization := .pending } with physicalCorroborated := false } with
          lifecycle := .failed }
  , descendantCase "terminal_result_edge"
      { { baseEdge with childRequestId := 8 } with lifecycle := .completed }
      (evidence := childEvidence .completed)
  , descendantCase "replicated_remote_materialization"
      { baseEdge with childRequestId := 9, materialization := .replicated }
  , descendantCase "unauthorized_principal"
      { { baseEdge with childRequestId := 10 } with ownerPrincipal := 99 }
  , descendantCase "unauthorized_session"
      { { baseEdge with childRequestId := 11 } with rootSessionId := 999 }
  , descendantCase "unauthorized_lineage"
      { { baseEdge with childRequestId := 12 } with lineageId := 9999 }
  , descendantCase "uncorroborated_materialized"
      { { baseEdge with childRequestId := 13 } with physicalCorroborated := false }
  , descendantCase "later_user_turn" baseEdge sessionOwner
  , descendantCase "other_conversation" baseEdge
      { sessionOwner with sessionId := "other" }
  , descendantCase "other_agent" baseEdge
      { sessionOwner with agentDid := "did:other" }
  , descendantCase "other_requester" baseEdge
      { sessionOwner with requesterDid := some "did:requester" }
  , descendantCase "empty_requester_is_not_absent" baseEdge
      { sessionOwner with requesterDid := some "" }
  , descendantCase "missing_agent" baseEdge
      { sessionOwner with agentDid := "" }
  , descendantCase "missing_session" baseEdge
      { sessionOwner with sessionId := "" }
  , descendantCase "blank_agent" baseEdge
      { sessionOwner with agentDid := " \t" }
  , descendantCase "blank_session" baseEdge
      { sessionOwner with sessionId := " \t" }
  , descendantCase "failed_child_continues"
      { { baseEdge with childRequestId := 15 } with lifecycle := .failed }
      (evidence := childEvidence .failed)
  , descendantCase "timed_out_child_continues"
      { { baseEdge with childRequestId := 16 } with lifecycle := .timedOut }
      (evidence := childEvidence .dead)
  , descendantCase "completed_child_continues"
      { { baseEdge with childRequestId := 17 } with lifecycle := .completed }
      (evidence := childEvidence .completed)
  , descendantCase "cancelled_child_not_resurrected"
      { { baseEdge with childRequestId := 18 } with lifecycle := .cancelled }
      (evidence := childEvidence .interrupted)
  , descendantCase "cancelled_bridge_with_failed_child"
      { { baseEdge with childRequestId := 19 } with lifecycle := .cancelled }
      (evidence := childEvidence .failed)
  , descendantCase "failed_foreground_child"
      { { { baseEdge with childRequestId := 20 } with lifecycle := .failed } with
          awaitMode := .foreground }
      (evidence := childEvidence .failed)
  , descendantCase "failed_nested_child"
      { { { { baseEdge with childRequestId := 21 } with lifecycle := .failed } with
          directFromRoot := false } with controlPrincipal := 11 }
      (evidence := childEvidence .failed)
  , descendantCase "failed_uncorroborated_child"
      { { { baseEdge with childRequestId := 22 } with lifecycle := .failed } with
          physicalCorroborated := false }
      (evidence := childEvidence .failed)
  , descendantCase "converging_bridge_finished_child"
      { baseEdge with childRequestId := 23 } (evidence := childEvidence .failed)
  , descendantCase "running_child_absent_row"
      { baseEdge with childRequestId := 24 }
      (evidence := { liveEvidence with child := none })
  , descendantCase "finished_child_absent_row"
      { { baseEdge with childRequestId := 25 } with lifecycle := .completed }
      (evidence := { liveEvidence with child := none })
  , descendantCase "unclaimed_spawn_late_child_failure"
      { { baseEdge with childRequestId := 26 } with lifecycle := .failed }
      (evidence := { childEvidence .failed with spawnUnclaimed := true, cancelIntent := true })
  , descendantCase "unclaimed_spawn_class_only"
      { { baseEdge with childRequestId := 27 } with lifecycle := .failed }
      (evidence := { childEvidence .failed with spawnUnclaimed := true })
  , descendantCase "settled_bridge_with_cancel_intent"
      { { baseEdge with childRequestId := 28 } with lifecycle := .failed }
      (evidence := { childEvidence .failed with cancelIntent := true })
  , descendantCase "finished_child_of_interrupted_parent_turn"
      { { baseEdge with childRequestId := 29 } with lifecycle := .completed }
      (evidence := childEvidence .completed)
  ]

/-- One edge of a cursor case: its durable identity and the lifecycle it has
    when the anchor is resolved. -/
structure DescendantCursorEdge where
  toolCallId : Nat
  childRequestId : Nat
  lifecycle : String
  deriving Repr

/-- `anchorSettled` records that the anchor was issued while its edge was
    running and resolved after the edge settled into its listed lifecycle. -/
structure DescendantCursorCase where
  name : String
  edges : List DescendantCursorEdge
  after : Option (Nat × Nat)
  anchorSettled : Bool
  expectedChildRequestIds : List Nat
  staleCursor : Bool
  deriving Repr

def cursorEdge (toolCallId childRequestId : Nat) (lifecycle : Lifecycle) : Edge :=
  { baseEdge with
      parentToolCallId := toolCallId
      childRequestId := childRequestId
      lifecycle := lifecycle }

def descendantCursorCase (name : String) (edges : List Edge)
    (after : Option Cursor) (anchorSettled : Bool := false) : DescendantCursorCase :=
  let result := DescendantGraph.page after edges
  { name
  , edges := edges.map fun edge =>
      { toolCallId := edge.parentToolCallId
      , childRequestId := edge.childRequestId
      , lifecycle := lifecycleString edge.lifecycle }
  , after
  , anchorSettled
  , expectedChildRequestIds := result.edges.map (·.childRequestId)
  , staleCursor := result.staleCursor }

def cursorFanOut : List Edge :=
  [cursorEdge 21 2 .running, cursorEdge 22 3 .running, cursorEdge 23 4 .running]

def descendantCursorCases : List DescendantCursorCase :=
  [ descendantCursorCase "no_anchor_lists_scope" cursorFanOut none
  , descendantCursorCase "anchor_resumes_after_edge" cursorFanOut (some (21, 2))
  , descendantCursorCase "anchor_at_last_edge_ends_scope" cursorFanOut (some (23, 4))
  , descendantCursorCase "unclaimed_failure_keeps_anchor"
      [cursorEdge 21 2 .running, cursorEdge 22 3 .failed, cursorEdge 23 4 .running]
      (some (22, 3)) true
  , descendantCursorCase "every_child_unclaimed_keeps_anchor"
      [cursorEdge 21 2 .failed, cursorEdge 22 3 .failed, cursorEdge 23 4 .failed]
      (some (21, 2)) true
  , descendantCursorCase "stale_anchor_restarts_scope" cursorFanOut (some (29, 9))
  , descendantCursorCase "stale_anchor_same_tool_other_child" cursorFanOut (some (21, 9))
  ]

/-- One explicit `cancel_subagent` over a child session holding the original
child request followed by steered requests, oldest first. -/
structure CancelChildSessionCase where
  name : String
  session : List String
  cancelled : List String
  deriving Repr

def cancelChildSessionCase (name : String) (session : List RequestState) :
    CancelChildSessionCase :=
  { name
  , session := session.map RequestState.toDefraDB
  , cancelled := (cancelChildSession session).map RequestState.toDefraDB }

def cancelChildSessionCases : List CancelChildSessionCase :=
  [ cancelChildSessionCase "finished_child_queued_steer" [.failed, .pending]
  , cancelChildSessionCase "finished_child_running_steer" [.completed, .processing]
  , cancelChildSessionCase "finished_child_running_and_queued_steers"
      [.completed, .processing, .pending]
  , cancelChildSessionCase "running_child_queued_steer" [.processing, .pending]
  , cancelChildSessionCase "finished_child_settled_steer" [.failed, .completed]
  ]

end Conformance.ContractCases
