import Proofs.Conformance.ContractCases.BridgeStep
import Proofs.RequestExecutionLease.Transition
import Proofs.Background.CancelAcknowledgement
import Proofs.Background.CompletionDelivery
import Proofs.Session.Properties.Executable

/-!
# R5 distributed scenario compositions

These traces replace the handwritten JSON scripts.  Their bridge outcomes are
computed by `Subagent.BridgedState.step`; this file only composes that owner
with explicit replication availability, delivery idempotence, session-keyed
wake coalescing, and stated crash/reopen premises.  Missing terminal request,
header, or segment facts leave an observer inert, so partitioned/reordered
arrival remains visible in the generated actions.
-/

namespace Conformance.ContractCases

inductive R5Node where | a | b deriving DecidableEq, Repr
inductive R5Terminal where | completed | failed | interrupted deriving DecidableEq, Repr

inductive R5ScenarioAction where
  | pair (node peer : R5Node)
  | acceptedBridge (tool child session : String) (parentDepth : Nat)
  | rejectedSpawnInvocation (tool child session : String) (parentDepth : Nat)
  | replicateBridge (tool : String) (source target : R5Node)
  | materializeChild (child tool : String)
  | beginChild (child : String) (generation : Nat)
  | awaitChildExpiry (child : String)
  | replicateChild (child : String) (source target : R5Node)
  | publishTerminal (child : String) (terminal : R5Terminal) (hasMessage : Bool)
  | replicateTerminalRequest (child : String) (source target : R5Node)
  | replicateOutputSegments (child : String) (source target : R5Node)
  | replicateMessageHeader (child : String) (source target : R5Node)
  | observeCompletion
  | cancelBridge (tool : String)
  | replicateCancelIntent (tool : String) (source target : R5Node)
  | mirrorCancel (tool : String)
  | observeCancelAck
  | recoverBridges
  | recoverChildRequests (expected fresh : Nat)
  | crash (node : R5Node) (durableReopenPremise : Bool)
  | advanceClock (node : R5Node) (seconds : Nat)
  | converge
  deriving Repr

structure R5BridgeFact where
  tool : String
  child : String
  session : String
  parentDepth : Nat
  state : ToolExecution.ToolCallState := .running
  cancel : Subagent.CancelAcknowledgement.Row := {}
  deriving Repr

structure R5ChildFact where
  child : String
  depth : Nat := 0
  execution : RequestExecutionLease.World Nat := RequestExecutionLease.initial Nat
  interruptRequested : Bool := false
  deriving DecidableEq, Repr

def R5ChildFact.terminal (child : R5ChildFact) : Option R5Terminal :=
  match child.execution.request with
  | .completed => some .completed
  | .failed => some .failed
  | .interrupted => some .interrupted
  | _ => none

structure R5ScenarioState where
  childLeaseSecs : Nat := 120
  observationsValid : Bool := true
  cancelAckThreshold : Nat := 300
  aNow : Time := 0
  bNow : Time := 0
  aBridges : List R5BridgeFact := []
  bBridges : List R5BridgeFact := []
  rejectedInvocations : List String := []
  aChildren : List R5ChildFact := []
  bChildren : List R5ChildFact := []
  aTerminalRequests : List String := []
  aSegments : List String := []
  aHeaders : List String := []
  deliveries : List (String × Subagent.CompletionDelivery.NotificationState) := []
  queues : List (String × SessionQueue.SessionQueueState) := []
  cancelAcks : List (String × Subagent.CancelAcknowledgement.Outcome) := []
  aGeneration : Nat := 0
  bGeneration : Nat := 0
  deriving Repr

/-- Projection of the delivery owner's durable receipts, keyed by child like
the native subagent notification key. Receipt-to-canonical-message refinement
remains the publication owner's/native adapter's obligation, not a Boolean
proof of storage atomicity or authorization. -/
def R5ScenarioState.notifications (s : R5ScenarioState) : List String :=
  (s.deliveries.filter fun row => row.2.notificationPresent).map Prod.fst

/-- Scenario symbols bind to finite session identities in first-observation
order. The native adapter binds these same symbols to real session documents;
this fixture does not decide ACP or native identity authentication. -/
def R5ScenarioState.wakeSessions (s : R5ScenarioState) : List String :=
  (s.queues.filter fun row => !row.2.pending.isEmpty).map Prod.fst

/-- Notification persistence precedes this call. Both fresh enqueue and reuse
of an existing pending session wake run through the executable queue owner. -/
def enqueueR5Wake (s : R5ScenarioState) (session : String) : R5ScenarioState :=
  let queue := match s.queues.find? (fun row => row.1 == session) with
    | some row => row.2
    | none =>
        { scope := ⟨1, s.queues.length + 1, none⟩
        , active := none, pending := [], terminal := ∅ }
  let wake : SessionQueue.QueueEntry :=
    { requestId := s.notifications.length, createdAt := s.aNow,
      source := .backgroundCompletion, policy := .coalesce,
      queueKey := some queue.sessionId, queuedAfter := none }
  match SessionQueue.step? queue (.coalescePending wake) with
  | none => { s with observationsValid := false }
  | some post =>
      let queues := if s.queues.any (fun row => row.1 == session) then
          s.queues.map (fun row => if row.1 == session then (session, post) else row)
        else s.queues ++ [(session, post)]
      { s with queues }

/-- Observe the durable winner, including cancellation, only after its child
dependencies have arrived. This is reconciliation, not a new terminal CAS.
Repeated delivery uses the existing idempotent receipt/marker owner. -/
def reconcileR5Notification (s : R5ScenarioState) (bridge : R5BridgeFact) :
    R5ScenarioState :=
  let before := match s.deliveries.find? (fun row => row.1 == bridge.child) with
    | some row => row.2
    | none => { terminal := false, notificationPresent := false, deliveryMarked := false }
  let delivered := Subagent.CompletionDelivery.reconcileDelivery
    { before with terminal := decide (isTerminal bridge.state) }
  let deliveries := if s.deliveries.any (fun row => row.1 == bridge.child) then
      s.deliveries.map (fun row =>
        if row.1 == bridge.child then (bridge.child, delivered) else row)
    else s.deliveries ++ [(bridge.child, delivered)]
  let post := { s with deliveries }
  if delivered.notificationPresent then enqueueR5Wake post bridge.session else post

def replaceBridge (rows : List R5BridgeFact) (next : R5BridgeFact) : List R5BridgeFact :=
  next :: rows.filter (fun row => row.tool != next.tool)

def replaceChild (rows : List R5ChildFact) (next : R5ChildFact) : List R5ChildFact :=
  next :: rows.filter (fun row => row.child != next.child)

def child? (rows : List R5ChildFact) (id : String) : Option R5ChildFact :=
  rows.find? (fun row => row.child == id)

/-- A native readiness observation, not an assumption that materialization
already ran the child. Both transitions use the request lease owner. -/
def beginR5Child (child : R5ChildFact) (generation duration : Nat) :
    Option R5ChildFact := do
  let claimed ← RequestExecutionLease.step? child.execution
    (.claim .mutationWriteGate generation duration (child.execution.now + duration))
  let running ← RequestExecutionLease.step? claimed (.begin .mutationWriteGate generation)
  pure { child with execution := running }

/-- The native adapter waits for the persisted deadline. Crash does not invoke
this observation and cannot make an otherwise live lease recoverable. -/
def awaitR5ChildExpiry (child : R5ChildFact) : Option R5ChildFact := do
  match child.execution.lease with
  | .active _ _ deadline =>
      let expired ← RequestExecutionLease.step? child.execution
        (.advanceTime (max child.execution.now deadline))
      pure { child with execution := expired }
  | _ => none

def recoverR5Child (child : R5ChildFact) (expected fresh : Nat) : R5ChildFact :=
  let outcome := if child.interruptRequested then
      RequestExecutionLease.Outcome.interrupted else .failed
  match RequestExecutionLease.step? child.execution
      (.recoverExpiredTerminal .mutationWriteGate expected fresh outcome) with
  | some recovered => { child with execution := recovered }
  | none => child

/-- Failed publication in the native harness is only an observation of the
recovery result. It cannot manufacture failure if recovery did not win. Normal
provider completion and observed interruption use the same finalization owner. -/
def observeR5Terminal (child : R5ChildFact) (terminal : R5Terminal)
    (hasMessage : Bool) : Option R5ChildFact := do
  if hasMessage != (terminal == .completed) then none
  match terminal with
  | .failed => if child.terminal == some .failed then some child else none
  | .completed | .interrupted =>
      if terminal == .interrupted && !child.interruptRequested then none
      if child.terminal == some terminal then some child else
      match child.execution.lease with
      | .active generation _ _ =>
          let outcome := if terminal == .completed then
              RequestExecutionLease.Outcome.completed else .interrupted
          let finished ← RequestExecutionLease.step? child.execution
            (.finalize .mutationWriteGate generation outcome)
          pure { child with execution := finished }
      | _ => none

def bridgeProjection? (bridge : R5BridgeFact) (terminal : R5Terminal) :
    Option ToolExecution.ToolCallState :=
  -- A locally cancelled bridge still owes a notification when its child
  -- becomes observable. Do not replay a running→terminal edge over that row.
  if isTerminal bridge.state then some bridge.state else
  let childState := match terminal with
    | .completed => RequestState.completed
    | .failed => RequestState.failed
    | .interrupted => RequestState.interrupted
  let event := match terminal with
    | .completed => Subagent.BridgedState.Event.bridge_complete
    | .failed | .interrupted => Subagent.BridgedState.Event.bridge_failure
  let fixture := bridgeStepFixture childState .processing .cascade true
  let fixture := { fixture with parent := { fixture.parent with
    tools := [{ bridgeStepToolRow .cascade .committed with state := bridge.state }] } }
  match Subagent.BridgedState.step fixture event with
  | none => none
  | some post => (post.parent.findToolByCallId 77).map fun tool => tool.state

def observeOne (s : R5ScenarioState) (bridge : R5BridgeFact) : R5ScenarioState :=
  match child? s.aChildren bridge.child with
  | none => s
  | some child =>
      if !(s.aTerminalRequests.contains bridge.child) then s else
      match child.terminal with
      | none => s
      | some terminal =>
          let dependencies := match terminal with
            | .completed => s.aSegments.contains bridge.child && s.aHeaders.contains bridge.child
            | .failed | .interrupted => true
          if !dependencies then s else
          match bridgeProjection? bridge terminal with
          | none => s
          | some projected =>
              let bridge := { bridge with state := projected }
              reconcileR5Notification
                { s with aBridges := replaceBridge s.aBridges bridge } bridge

def observeAll (s : R5ScenarioState) : R5ScenarioState :=
  s.aBridges.foldl observeOne s

/-- The coordinator's running-tool recovery projects terminal linked children
and their completion side effects. It does not repair an already-terminal
child-linked bridge's missing receipt; the completion reconciler owns that.
Foreign coordinator bridges on B are outside B's locally-owned sweep. -/
def recoverR5Bridges (s : R5ScenarioState) : R5ScenarioState :=
  s.aBridges.foldl (fun state bridge =>
    if bridge.state == .running then observeOne state bridge else state) s

def cancelR5Bridge (now : Time) (row : R5BridgeFact) : R5BridgeFact :=
  let tool := { bridgeStepToolRow .cascade .committed with state := row.state }
  match ToolExecution.ToolCallContext.step? tool (.cancelDuringRun .interrupted) with
  | none => row
  | some cancelled =>
      { row with
        state := cancelled.state
        cancel := ⟨true, some now, none⟩ }

def mirrorR5Cancel (bridge : R5BridgeFact) (child : R5ChildFact) : R5ChildFact :=
  let fixture := bridgeStepFixture child.execution.request .processing .cascade true
  let fixture := { fixture with parent := { fixture.parent with
    tools := [{ bridgeStepToolRow .cascade .committed with state := bridge.state }] } }
  match Subagent.BridgedState.step fixture .bridge_cancel_cascade with
  | none => child
  | some mirrored => { child with
      interruptRequested := mirrored.child.request.interruptRequestedAt.isSome }

def observeR5CancelAcks (s : R5ScenarioState) : R5ScenarioState :=
  s.aBridges.foldl (fun state bridge =>
    let child := child? state.aChildren bridge.child
    let observation : Subagent.CancelAcknowledgement.Observation :=
      { locallyOwned := true
      , childState := child.map (·.execution.request)
      , childInterruptRequested := child.any (·.interruptRequested)
      , now := state.aNow }
    match Subagent.CancelAcknowledgement.observe state.cancelAckThreshold
        bridge.cancel observation with
    | none => state
    | some result => { state with
        aBridges := replaceBridge state.aBridges { bridge with cancel := result.row }
        cancelAcks := state.cancelAcks ++ [(bridge.tool, result.outcome)] }) s

/-- A depth-rejected provider call is still a durable tool invocation, not a
    child-linked background bridge. Its pending → failed result is computed by
    the single-row tool owner; the delegation guard rejects creation at the
    maximum depth. -/
def rejectedSpawnPost? : Option ToolExecution.ToolCallContext :=
  ToolExecution.ToolCallContext.step?
    { bridgeStepToolRow .cascade .committed with
        state := .pending, awaitMode := .foreground, childRequestId := none }
    (.spawnFailed .argumentInvalid)

def R5ScenarioState.step (s : R5ScenarioState) : R5ScenarioAction → R5ScenarioState
  | .pair _ _ => s
  | .acceptedBridge tool child session parentDepth =>
      if parentDepth < Subagent.maxSubagentDepth then
        { s with aBridges := replaceBridge s.aBridges { tool, child, session, parentDepth } }
      else s
  | .rejectedSpawnInvocation tool _ _ parentDepth =>
      if parentDepth < Subagent.maxSubagentDepth then s else
      match rejectedSpawnPost? with
      | some post => if post.state = .failed ∧ post.failureClass = some .argumentInvalid then
          { s with rejectedInvocations := s.rejectedInvocations ++ [tool] }
        else s
      | none => s
  | .replicateBridge tool .a .b =>
      match s.aBridges.find? (fun row => row.tool == tool) with
      | some row => { s with bBridges := replaceBridge s.bBridges row }
      | none => s
  | .replicateBridge _ _ _ => s
  | .materializeChild child tool =>
      match s.bBridges.find? (fun row => row.tool == tool) with
      | some bridge => if bridge.parentDepth < Subagent.maxSubagentDepth then
          { s with bChildren := replaceChild s.bChildren { child, depth := bridge.parentDepth + 1 } }
        else s
      | none => s
  | .beginChild child generation =>
      match child? s.bChildren child with
      | some row => match beginR5Child row generation s.childLeaseSecs with
        | some running => { s with bChildren := replaceChild s.bChildren running }
        | none => { s with observationsValid := false }
      | none => { s with observationsValid := false }
  | .awaitChildExpiry child =>
      match child? s.bChildren child with
      | some row => match awaitR5ChildExpiry row with
        | some expired => { s with bChildren := replaceChild s.bChildren expired }
        | none => { s with observationsValid := false }
      | none => { s with observationsValid := false }
  | .replicateChild child .b .a =>
      match child? s.bChildren child with
      | some row => { s with aChildren := replaceChild s.aChildren row }
      | none => s
  | .replicateChild _ _ _ => s
  | .publishTerminal child terminal hasMessage =>
      match child? s.bChildren child with
      | some row => match observeR5Terminal row terminal hasMessage with
        | some finished => { s with bChildren := replaceChild s.bChildren finished }
        | none => { s with observationsValid := false }
      | none => { s with observationsValid := false }
  | .replicateTerminalRequest child .b .a =>
      match child? s.bChildren child with
      | some row => { s with
          aChildren := replaceChild s.aChildren row
          aTerminalRequests := if s.aTerminalRequests.contains child then s.aTerminalRequests else s.aTerminalRequests ++ [child] }
      | none => s
  | .replicateTerminalRequest _ _ _ => s
  | .replicateOutputSegments child .b .a =>
      { s with aSegments := if s.aSegments.contains child then s.aSegments else s.aSegments ++ [child] }
  | .replicateOutputSegments _ _ _ => s
  | .replicateMessageHeader child .b .a =>
      { s with aHeaders := if s.aHeaders.contains child then s.aHeaders else s.aHeaders ++ [child] }
  | .replicateMessageHeader _ _ _ => s
  | .observeCompletion => observeAll s
  | .cancelBridge tool =>
      { s with aBridges := s.aBridges.map fun row =>
          if row.tool == tool then cancelR5Bridge s.aNow row else row }
  | .replicateCancelIntent tool .a .b =>
      match s.aBridges.find? (fun row => row.tool == tool) with
      | some row => { s with bBridges := replaceBridge s.bBridges row }
      | none => s
  | .replicateCancelIntent _ _ _ => s
  | .mirrorCancel tool =>
      match s.bBridges.find? (fun row => row.tool == tool && row.cancel.pending) with
      | none => s
      | some bridge => { s with bChildren := s.bChildren.map fun child =>
          if child.child == bridge.child && child.terminal.isNone then (mirrorR5Cancel bridge child) else child }
  | .observeCancelAck => observeR5CancelAcks s
  | .recoverChildRequests expected fresh =>
      { s with bChildren := s.bChildren.map (recoverR5Child · expected fresh) }
  | .recoverBridges => recoverR5Bridges s
  | .crash .a true => { s with aGeneration := s.aGeneration + 1 }
  | .crash .b true => { s with bGeneration := s.bGeneration + 1 }
  | .crash _ false => s
  | .advanceClock .a seconds => { s with aNow := s.aNow + seconds }
  | .advanceClock .b seconds => { s with bNow := s.bNow + seconds }
  | .converge => observeR5CancelAcks (observeAll s)

def foldR5Scenario (actions : List R5ScenarioAction) (childLeaseSecs : Nat := 120) :
    R5ScenarioState :=
  actions.foldl R5ScenarioState.step { childLeaseSecs }

structure R5ScenarioCase where
  name : String
  actions : List R5ScenarioAction
  childLeaseSecs : Nat
  post : R5ScenarioState
  deriving Repr

def r5Case (name : String) (actions : List R5ScenarioAction)
    (childLeaseSecs : Nat := 120) : R5ScenarioCase :=
  { name, actions, childLeaseSecs, post := foldR5Scenario actions childLeaseSecs }

/-- Immediate recovery boundaries, before any later completion observation
can conceal a missing recovery effect. Indices are zero-based action indices. -/
def r5RecoveryCheckpoints (scenario : R5ScenarioCase) : List (Nat × R5ScenarioState) :=
  scenario.actions.zipIdx.filterMap fun (action, index) =>
    match action with
    | .recoverBridges | .recoverChildRequests _ _ =>
        some (index, foldR5Scenario (scenario.actions.take (index + 1)) scenario.childLeaseSecs)
    | _ => none

def setup (tool child session : String) : List R5ScenarioAction :=
  [ .pair .a .b, .pair .b .a, .acceptedBridge tool child session 0,
    .replicateBridge tool .a .b, .materializeChild child tool,
    .beginChild child 0,
    .replicateChild child .b .a ]

def complete (child : String) : List R5ScenarioAction :=
  [ .publishTerminal child .completed true,
    .replicateTerminalRequest child .b .a,
    .replicateOutputSegments child .b .a,
    .replicateMessageHeader child .b .a ]

def r5ScenarioCases : List R5ScenarioCase :=
  [ r5Case "happy_path"
      (setup "tool-call-1" "child-req-1" "parent-req-1-session" ++
       complete "child-req-1" ++ [.observeCompletion, .converge])
  , r5Case "b_crash_mid_execution"
      (setup "tool-call-b-crash" "child-req-b-crash" "parent-req-b-crash-session" ++
       [.crash .b true, .awaitChildExpiry "child-req-b-crash", .recoverChildRequests 0 1,
        .publishTerminal "child-req-b-crash" .failed false,
        .replicateTerminalRequest "child-req-b-crash" .b .a, .observeCompletion, .converge]) 2
  , r5Case "a_crash_mid_wait"
      (setup "tool-call-a-crash-before" "child-req-a-crash-before" "parent-before-session" ++
       [.crash .a true] ++ complete "child-req-a-crash-before" ++
       [.recoverBridges, .observeCompletion] ++
       setup "tool-call-a-crash-after" "child-req-a-crash-after" "parent-after-session" ++
       complete "child-req-a-crash-after" ++
       [.crash .a true, .recoverBridges, .observeCompletion, .converge])
  , r5Case "partition_during_cancel"
      (setup "tool-call-cancel" "child-req-cancel" "parent-cancel-session" ++
       [.cancelBridge "tool-call-cancel", .observeCancelAck,
        .advanceClock .a 360, .observeCancelAck, .observeCancelAck,
        .replicateCancelIntent "tool-call-cancel" .a .b, .mirrorCancel "tool-call-cancel",
        .publishTerminal "child-req-cancel" .interrupted false,
        .replicateTerminalRequest "child-req-cancel" .b .a, .observeCancelAck, .converge])
  , r5Case "multi_completion_coalesce"
      (setup "tool-call-multi-1" "child-req-multi-1" "parent-multi-session" ++
       [.acceptedBridge "tool-call-multi-2" "child-req-multi-2" "parent-multi-session" 0,
        .replicateBridge "tool-call-multi-2" .a .b,
        .materializeChild "child-req-multi-2" "tool-call-multi-2",
        .beginChild "child-req-multi-2" 0,
        .replicateChild "child-req-multi-2" .b .a] ++
       complete "child-req-multi-1" ++ complete "child-req-multi-2" ++
       [.observeCompletion, .observeCompletion, .converge])
  , r5Case "remote_depth_ceiling"
      [ .pair .a .b, .pair .b .a,
        .rejectedSpawnInvocation "tool-call-depth-ceiling" "child-req-depth-ceiling"
          "parent-depth-ceiling-session" Subagent.maxSubagentDepth,
        .converge ]
  ]

theorem r5ScenarioCases_owner_computed :
    (r5ScenarioCases.map fun c =>
      (c.name, c.post.notifications.length, c.post.wakeSessions.length,
       c.post.aGeneration, c.post.bGeneration)) =
    [ ("happy_path", 1, 1, 0, 0)
    , ("b_crash_mid_execution", 1, 1, 0, 1)
    , ("a_crash_mid_wait", 2, 2, 2, 0)
    , ("partition_during_cancel", 1, 1, 0, 0)
    , ("multi_completion_coalesce", 2, 1, 0, 0)
    , ("remote_depth_ceiling", 0, 0, 0, 0) ] := by
  native_decide

theorem r5ScenarioCases_observations_valid :
    r5ScenarioCases.all (fun scenario =>
      scenario.childLeaseSecs > 0 && scenario.post.observationsValid) = true := by
  native_decide

set_option synthInstance.maxSize 256 in
theorem r5_recovery_effects_precede_later_completion_observation :
    (r5ScenarioCases.map fun scenario =>
      (scenario.name, (r5RecoveryCheckpoints scenario).map fun checkpoint =>
        (checkpoint.2.notifications.length, checkpoint.2.wakeSessions.length,
         checkpoint.2.bChildren.map (·.terminal)))) =
    [("happy_path", []),
     ("b_crash_mid_execution", [(0, 0, [some .failed])]),
     ("a_crash_mid_wait", [(1, 1, [some .completed]),
       (2, 2, [some .completed, some .completed])]),
     ("partition_during_cancel", []), ("multi_completion_coalesce", []),
     ("remote_depth_ceiling", [])] := by
  native_decide

/-- Finite bindings are checked at every scenario prefix, not assumed from
their final counts. A child names one bridge/receipt, and each session symbol
names one distinct queue scope. This is fixture identity, not an ACP proof. -/
def r5BindingsValid (state : R5ScenarioState) : Prop :=
  (state.aBridges.map (·.tool)).Nodup ∧
  (state.aBridges.map (·.child)).Nodup ∧
  (state.deliveries.map Prod.fst).Nodup ∧
  (state.queues.map Prod.fst).Nodup ∧
  (state.queues.map (fun row => row.2.sessionId)).Nodup ∧
  (∀ row ∈ state.deliveries,
    Subagent.CompletionDelivery.DeliveryInvariant row.2 ∧
    ∃ bridge ∈ state.aBridges, bridge.child = row.1) ∧
  (∀ row ∈ state.queues,
    row.2.pending.length ≤ 1 ∧
    (∃ bridge ∈ state.aBridges, bridge.session = row.1 ∧
      ∃ receipt ∈ state.deliveries,
        receipt.1 = bridge.child ∧ receipt.2.notificationPresent = true) ∧
    ∀ wake ∈ row.2.pending,
      wake.source = .backgroundCompletion ∧ wake.coalesceWellFormed row.2.sessionId)

instance (state : R5ScenarioState) : Decidable (r5BindingsValid state) := by
  unfold r5BindingsValid Subagent.CompletionDelivery.DeliveryInvariant
  infer_instance

theorem r5ScenarioCases_every_prefix_has_exact_bindings :
    ∀ scenario ∈ r5ScenarioCases,
      ∀ count ∈ List.range (scenario.actions.length + 1),
        r5BindingsValid (foldR5Scenario (scenario.actions.take count) scenario.childLeaseSecs) := by
  native_decide

theorem r5_terminal_bridge_is_not_overwritten
    (bridge : R5BridgeFact) (terminal : R5Terminal)
    (closed : isTerminal bridge.state) :
    bridgeProjection? bridge terminal = some bridge.state := by
  simp [bridgeProjection?, closed]

theorem r5ScenarioCases_converged_delivery_and_queue_replay_is_inert :
    r5ScenarioCases.all (fun scenario =>
      let again := scenario.post.step .converge
      decide (again.deliveries = scenario.post.deliveries) &&
      decide (again.queues = scenario.post.queues)) = true := by
  native_decide

theorem r5_partition_acknowledgement_is_owner_computed :
    (r5ScenarioCases.map fun scenario => (scenario.name, scenario.post.cancelAcks)) =
      [("happy_path", []), ("b_crash_mid_execution", []), ("a_crash_mid_wait", []),
       ("partition_during_cancel",
        [("tool-call-cancel", .pending), ("tool-call-cancel", .stuck),
         ("tool-call-cancel", .pending), ("tool-call-cancel", .acked)]),
       ("multi_completion_coalesce", []), ("remote_depth_ceiling", [])] := by
  native_decide

theorem r5_crash_preserves_child_leases (state : R5ScenarioState) :
    (state.step (.crash .b true)).bChildren = state.bChildren := rfl

theorem r5_live_child_is_not_recovered (child : R5ChildFact)
    (owner duration deadline expected fresh : Nat)
    (lease : child.execution.lease = .active owner duration deadline)
    (live : child.execution.now < deadline) :
    recoverR5Child child expected fresh = child := by
  simp [recoverR5Child, RequestExecutionLease.step?, lease,
    RequestExecutionLease.effectiveExpiry, not_le_of_gt live]

theorem r5_failed_observation_requires_recovered_state (child post : R5ChildFact)
    (observed : observeR5Terminal child .failed false = some post) :
    post = child ∧ child.terminal = some .failed := by
  simp only [observeR5Terminal] at observed
  split at observed
  · contradiction
  · split at observed
    · simp_all
    · contradiction

theorem r5_recovery_requires_expiry_and_failed_observation_cannot_invent_it :
    let child : R5ChildFact := { child := "child" }
    let running := (beginR5Child child 0 2).getD child
    (recoverR5Child running 0 1).execution = running.execution ∧
    observeR5Terminal running .failed false = none ∧
    ((awaitR5ChildExpiry running).map (recoverR5Child · 0 1)).map
      (·.terminal) = some (some .failed) := by
  native_decide

theorem r5ScenarioCases_remote_depth_bounded :
    ∀ scenario ∈ r5ScenarioCases,
      ∀ child ∈ scenario.post.bChildren,
        child.depth ≤ Subagent.maxSubagentDepth := by
  native_decide

theorem r5ScenarioCases_depth_rejects_invocation_without_bridge_or_child :
    let depthCase := (r5ScenarioCases.find? fun c => c.name = "remote_depth_ceiling").getD
      (r5Case "missing" [])
    depthCase.post.rejectedInvocations = ["tool-call-depth-ceiling"] ∧
    depthCase.post.aBridges.length = 0 ∧ depthCase.post.bBridges.length = 0 ∧
    depthCase.post.aChildren.length = 0 ∧ depthCase.post.bChildren.length = 0 := by
  native_decide

end Conformance.ContractCases
