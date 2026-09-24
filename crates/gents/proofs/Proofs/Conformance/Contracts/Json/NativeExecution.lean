import Proofs.CanonicalOutput.Execution.GateCases
import Proofs.CanonicalOutput.Execution.CompactionCases
import Proofs.CanonicalOutput.Execution.AuxiliaryCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.RequestExecutionLease

namespace Conformance.NativeExecutionContracts

open CanonicalOutput
open CanonicalOutput.Execution
open CanonicalOutput.Execution.Examples
open CanonicalOutput.Execution.Gate
open CanonicalOutput.Execution.Gate.Cases
open Conformance.Contracts

/-- Compact native inputs. Each constructor names an existing execution-owner
operation and its externally supplied fence values; fixture documents are the
canonical `Execution.Examples` documents, not an encoded copy of `World`. -/
inductive Input where
  | acceptForeground
  | acceptRemote
  | acceptRemoteBehaviorDrift
  | acceptRemoteWorkspaceDrift
  | realSpawnAccept
  | realSpawnBehaviorDrift
  | realSpawnWorkspaceDrift
  | dispatch (now : Nat)
  | closeForeground
  | completeForeground
  | deliverForeground
  | terminalizeCompleted
  | recover (actor : Nat) (now fresh deadline : Nat)
  | renew (now expectedDeadline : Nat)
  | appendRaw (actor now : Nat) (record : Segment)
  | closeAuxiliary (now generation : Nat) (closing : Segment)
  | appendToolOutput (actor now document : Nat) (record : Segment)
  | recoverItems (actor now fresh deadline : Nat) (items : List RecoveryItem)
  | recoverTerminal (actor now fresh : Nat)
      (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
      (items : List RecoveryItem)
  | closePartial (now generation : Nat) (item : RecoveryItem)
  | acceptTurn (closing : Segment) (message : MessageEnvelope)
      (admissions : List ToolAdmission)
  | dispatchCall (now call : Nat)
  | backgroundTool
  | backgroundReceipt
  | admitSpawned (admission : SpawnedToolAdmission)
  | publishAuthored (closing : Segment) (message : MessageEnvelope)
  | compact (cursor : Nat)
  | deliverResult (sequence : Nat)
  | appendWhileSiblingWaits (record : Segment)
  | revokeDead (actor now fresh : Nat)
  /-- A distinct immutable fact arriving by replication. It bypasses the local
  gate exactly as the contract says a remote merge does, so it is not an
  `Operation` and cannot be rejected by the execution owner. -/
  | replicate (record : Segment)

/-- What one script step does to the modeled world. -/
inductive Step where
  | commit (operation : Operation)
  /-- Same-task acquisition whose sibling then awaits the gate: the only poller
  of the holder is suspended, so the write cannot commit. -/
  | commitWhileSiblingWaits (operation : Operation)
  | replicate (record : Segment)

def Input.step : Input → Step
  | .acceptForeground =>
      .commit (.accept 7 providerTurn providerMessage [] [foregroundAdmission])
  | .acceptRemote =>
      .commit (.accept 7 providerTurn providerMessage [remote] [remoteAdmission])
  | .acceptRemoteBehaviorDrift =>
      .commit (.accept 7 providerTurn providerMessage [remote] [driftedRemoteAdmission])
  | .acceptRemoteWorkspaceDrift =>
      .commit (.accept 7 providerTurn providerMessage [remote] [driftedWorkspaceAdmission])
  | .realSpawnAccept =>
      .commit (.accept 7 realSpawnProviderTurn realSpawnProviderMessage [remote] [remoteAdmission])
  | .realSpawnBehaviorDrift =>
      .commit (.accept 7 realSpawnProviderTurn realSpawnProviderMessage [remote] [driftedRemoteAdmission])
  | .realSpawnWorkspaceDrift =>
      .commit (.accept 7 realSpawnProviderTurn realSpawnProviderMessage [remote] [driftedWorkspaceAdmission])
  | .dispatch _ => .commit (.dispatch 7 permit)
  | .closeForeground => .commit (.toolClose 600 (.native .complete) toolOutputClose)
  | .completeForeground => .commit
      (.toolComplete 600 (.native .complete) toolOutputClose (foregroundResultMessage 1))
  | .deliverForeground => .commit (.toolDeliver 600 (foregroundResultMessage 1))
  | .terminalizeCompleted => .commit (.terminalize 7 .completed (.message 501))
  | .recover _ _ fresh deadline => .commit (.recover 7 fresh 5 deadline [])
  | .renew _ expectedDeadline => .commit (.renew 7 expectedDeadline)
  | .appendRaw _ _ record => .commit (.append 7 record)
  | .closeAuxiliary _ generation closing => .commit (.closeAuxiliary generation closing)
  | .appendToolOutput _ _ document record => .commit (.toolAppend document record)
  | .recoverItems _ _ fresh deadline items => .commit (.recover 7 fresh 5 deadline items)
  | .recoverTerminal _ _ fresh outcome selection items =>
      .commit (.recoverTerminal 7 fresh outcome selection items)
  | .closePartial _ generation item => .commit (.closePartial generation item)
  | .acceptTurn closing message admissions =>
      .commit (.accept 7 closing message [] admissions)
  | .dispatchCall _ call => .commit (.dispatch 7 ⟨call, true, true⟩)
  | .backgroundTool => .commit (.toolControl 7 600 .background)
  | .backgroundReceipt =>
      .commit (.backgroundReceipt 600 backgroundReceiptClose backgroundReceiptMessage)
  | .admitSpawned admission => .commit (.admitSpawned 7 admission)
  | .publishAuthored closing message => .commit (.authored 7 closing message)
  | .compact cursor => .commit (.compact cursor)
  | .deliverResult sequence => .commit (.toolDeliver 600 (foregroundResultMessage sequence))
  | .revokeDead _ _ fresh => .commit (.revoke 7 fresh .dead (.message 501))
  | .appendWhileSiblingWaits record => .commitWhileSiblingWaits (.append 7 record)
  | .replicate record => .replicate record

def Input.actor : Input → Nat
  | .recover actor .. => actor
  | .appendRaw actor .. => actor
  | .appendToolOutput actor .. => actor
  | .recoverItems actor .. => actor
  | .recoverTerminal actor .. => actor
  | .revokeDead actor .. => actor
  | _ => 1

def Input.now : Input → Nat
  | .dispatch now => now
  | .recover _ now .. => now
  | .renew now _ => now
  | .appendRaw _ now _ => now
  | .closeAuxiliary now .. => now
  | .appendToolOutput _ now .. => now
  | .recoverItems _ now .. => now
  | .recoverTerminal _ now .. => now
  | .closePartial now .. => now
  | .dispatchCall now _ => now
  | .revokeDead _ now _ => now
  | _ => 5

def Input.tag : Input → String
  | .acceptForeground => "accept_foreground"
  | .acceptRemote => "accept_remote"
  | .acceptRemoteBehaviorDrift => "accept_remote"
  | .acceptRemoteWorkspaceDrift => "accept_remote"
  | .realSpawnAccept => "accept_remote"
  | .realSpawnBehaviorDrift => "accept_remote"
  | .realSpawnWorkspaceDrift => "accept_remote"
  | .dispatch _ => "dispatch"
  | .closeForeground => "close_foreground_tool"
  | .completeForeground => "complete_foreground_tool"
  | .deliverForeground => "deliver_foreground_result"
  | .terminalizeCompleted => "terminalize_completed"
  | .recover .. => "recover_expired_generation"
  | .renew .. => "renew_lease"
  | .appendRaw .. => "append_output"
  | .closeAuxiliary .. => "close_auxiliary"
  | .appendToolOutput .. => "append_tool_output"
  | .recoverItems .. => "recover_expired_generation"
  | .recoverTerminal .. => "recover_expired_terminal"
  | .closePartial .. => "close_partial"
  | .acceptTurn .. => "accept_turn"
  | .dispatchCall .. => "dispatch"
  | .backgroundTool => "background_tool"
  | .backgroundReceipt => "publish_background_receipt"
  | .admitSpawned _ => "admit_spawned_background"
  | .publishAuthored .. => "publish_authored"
  | .compact _ => "advance_compaction_cursor"
  | .deliverResult _ => "deliver_foreground_result"
  | .revokeDead .. => "revoke_corrupt"
  | .appendWhileSiblingWaits _ => "append_output_while_sibling_waits"
  | .replicate _ => "deliver_replicated_segment"

structure Observation where
  accepted : Bool
  generation : Option Nat
  terminalGeneration : Option Nat
  requestState : String
  toolState : Option String
  toolStuckSince : Option Nat
  toolCancelIntentAt : Option Nat
  inFlight : Bool
  nextSequence : Nat
  acceptedSequence : Option Nat
  physicalToolRequest : Option Nat
  leaseDeadline : Option Nat
  compactionCursor : Option Nat
  segments : List Segment
  messages : List MessageEnvelope
  deriving DecidableEq

def normalizedSegments (segments : List Segment) : List Segment :=
  segments.mergeSort fun left right =>
    (left.id, canonicalSegmentJson left) ≤ (right.id, canonicalSegmentJson right)

def normalizedMessages (messages : List MessageEnvelope) : List MessageEnvelope :=
  messages.mergeSort fun left right =>
    (left.header.id, canonicalMessageJson left) ≤ (right.header.id, canonicalMessageJson right)

def observe (document : Nat) (accepted : Bool) (world : World) : Observation :=
  let tool := ownedToolByDocument? world document
  { accepted
    generation := world.currentGeneration?
    terminalGeneration := match world.lease.lease with
      | .terminal generation _ => some generation
      | _ => none
    requestState := world.lease.request.toDefraDB
    toolState := tool.map (ToolExecution.ToolCallState.toDefraDB ·.context.state)
    toolStuckSince := tool.bind (·.stuckSince)
    toolCancelIntentAt := tool.bind (·.cancelCascadeIntentAt)
    inFlight := document ∈ world.transcript.inFlight
    nextSequence := world.transcript.nextSeq
    acceptedSequence := tool.map (·.acceptedSequence)
    physicalToolRequest := tool.map (·.requestDoc)
    leaseDeadline := match world.lease.lease with
      | .active _ _ deadline | .recoverable _ _ deadline => some deadline
      | _ => none
    compactionCursor := world.compactionCursor
    segments := normalizedSegments world.segments
    messages := normalizedMessages world.messages }

/-- Release through the modeled scheduling owner. A holder whose commit was
rejected is still in the storage phase: rollback must be observed returning
before the gate can be released. Rejection is never an implicit unlock. -/
def releaseFor (world : World) (actor : Nat) : Option World :=
  if world.gateOwner.isNone then some world
  else do
    let returned ←
      if world.gateSchedule.phase == .storage then scheduling world actor .storageReturned
      else some world
    scheduling returned actor .release

/-- Run each input through the real modeled gate. A rejected operation records
`accepted = false` and leaves the durable world unchanged, matching rollback.
A replicated fact is delivered by the same exact-fact union the model uses for
remote merge; it never acquires the local gate. -/
def runStep (document : Nat) (world : World) (previousActor : Nat)
    (input : Input) : Option (World × Nat × Observation) := do
  match input.step with
  | .replicate record =>
      let after := { world with segments := deliver world.segments record }
      some (after, previousActor, observe document true after)
  | .commitWhileSiblingWaits operation =>
      let released ← releaseFor world previousActor
      let held ← acquire released input.actor false
      let suspended ← scheduling held input.actor .siblingWait
      match commit suspended input.actor input.now operation with
      | some after => some (after, input.actor, observe document true after)
      | none => some (suspended, input.actor, observe document false suspended)
  | .commit operation =>
      let released ← releaseFor world previousActor
      let held ← acquire released input.actor true
      match commit held input.actor input.now operation with
      | some after => some (after, input.actor, observe document true after)
      | none => some (held, input.actor, observe document false held)

def run (seed : World) (document : Nat) (inputs : List Input) : Option (List Observation) := do
  let (_, _, observations) ← inputs.foldlM (fun (world, actor, observations) input => do
    let (after, nextActor, observation) ← runStep document world actor input
    pure (after, nextActor, observations ++ [observation])) (initial seed, 1, [])
  pure observations

structure Case where
  name : String
  seed : World
  queryDocument : Nat
  inputs : List Input
  expected : Option (List Observation)
  nativeGap : Option String

def mkCaseFor (name : String) (world : World) (document : Nat)
    (inputs : List Input) : Case :=
  ⟨name, world, document, inputs, run world document inputs, none⟩

def mkCase (name : String) (world : World) (inputs : List Input) : Case :=
  mkCaseFor name world 600 inputs

def mkModelCase (name : String) (world : World) (inputs : List Input)
    (nativeGap : String) : Case :=
  { mkCase name world inputs with nativeGap := some nativeGap }

def recoveryItems : List RecoveryItem :=
  [RecoveryItem.mk (partialClose 101 0 1 10) (some (recoveryMessage 200 101 0 10))]

def lateStaleFlush : Segment :=
  { raw 102 0 1 11 with flush := some ⟨1, [⟨0, 1, none⟩], [66]⟩ }

/-- Same-generation partial publication uses the same recovery presentation,
but the live producer remains generation 7. -/
def livePartialItem : RecoveryItem :=
  let message := recoveryMessage 200 101 0 5
  ⟨partialClose 101 0 1 5,
    some { message with header := { message.header with publication := .requestRecovery 7 } }⟩

def livePartialCases : List Case :=
  [ mkCase "live_partial_publication_replays_after_late_raw_arrival" (world 5)
      [.appendRaw 1 5 (raw 100 0 0 5), .closePartial 5 7 livePartialItem,
       .replicate lateStaleFlush, .closePartial 6 7 livePartialItem]
  , mkCase "live_partial_rejects_stale_generation" (world 5)
      [.appendRaw 1 5 (raw 100 0 0 5), .closePartial 5 8 livePartialItem]
  , mkCase "live_partial_rejects_short_extent" (world 5)
      [.appendRaw 1 5 (raw 100 0 0 5),
       .closePartial 5 7 { livePartialItem with closing := partialClose 101 0 0 5 }]
  ]

example : livePartialCases.map (fun value => value.expected.map
    (List.map (·.accepted))) =
    [some [true, true, true, true], some [true, false], some [true, false]] := by
  native_decide

example : ((livePartialCases.head?.bind (·.expected)).bind List.getLast?).map
    (fun observation => (observation.messages.length, observation.nextSequence,
      observation.generation, observation.leaseDeadline)) = some (1, 1, some 7, some 10) := by
  native_decide

def compactionBoundary : MessageEnvelope :=
  CanonicalOutput.Execution.Compaction.Examples.boundary

/-- The four original tool-seam scripts. -/
def toolSeamCases : List Case :=
  [ mkModelCase "pending_remote_recovery_cancels_before_dispatch"
      (routedWorld 5) [.acceptRemote, .recover 2 10 8 20, .dispatch 10]
      "The abstract nativeCommand child and resume-to-active recovery are model primitives; product terminal recovery of a real spawn_subagent is covered separately."
  , mkCase "real_spawn_pending_terminal_recovery_cancels_before_dispatch"
      (routedWorld 5) [.realSpawnAccept,
        .recoverTerminal 2 10 8 .failed (.message 501) [], .dispatch 10]
  , mkModelCase "running_foreground_recovery_records_handoff"
      (world 5) [.acceptForeground, .dispatch 5, .recover 2 10 8 20]
      "The resume-to-active recovery primitive has no product transaction; native terminal recovery separately checks the running-tool handoff."
  , mkCase "running_foreground_terminal_recovery_records_handoff"
      (world 5) [.acceptForeground, .dispatch 5,
        .recoverTerminal 2 10 8 .failed (.message 501) []]
  , mkCase "running_foreground_terminalization_rejected"
      (world 5) [.acceptForeground, .dispatch 5, .terminalizeCompleted]
  , mkModelCase "foreground_close_delivery_then_terminalization"
      (world 5) [.acceptForeground, .dispatch 5, .closeForeground,
        .deliverForeground, .terminalizeCompleted]
      "Native complete_raw_with_presentation closes and delivers in one transaction; use foreground_tool_completion_atomically_pairs_result for native parity." ]

/-- Lease ordering: only explicit due renewal moves the deadline; output and
dispatch do not, and a superseded writer is inert. -/
def leaseOrderingCases : List Case :=
  [ mkModelCase "renewal_wins_before_recovery"
      (world 5) [.renew 8 10, .recover 2 10 8 20]
      "The resume-to-active recovery primitive has no product transaction; native terminal recovery checks the same renewal fence."
  , mkCase "renewal_wins_before_terminal_recovery"
      (world 5) [.renew 8 10,
        .recoverTerminal 2 10 8 .failed .noMessage []]
  , mkModelCase "output_append_keeps_deadline_then_recovery_swaps"
      (world 5) [.appendRaw 1 5 (raw 100 0 0 5), .recoverItems 2 10 8 20 recoveryItems]
      "The resume-to-active recovery primitive has no product transaction; native terminal recovery checks that append left the expiry unchanged."
  , mkCase "output_append_keeps_deadline_then_terminal_recovery"
      (world 5) [.appendRaw 1 5 (raw 100 0 0 5),
        .recoverTerminal 2 10 8 .failed (.message 200) recoveryItems]
  , mkModelCase "stale_append_rejected_after_recovery"
      (world 5) [.appendRaw 1 5 (raw 100 0 0 5), .recoverItems 2 10 8 20 recoveryItems,
        .appendRaw 1 11 lateStaleFlush]
      "The resume-to-active recovery primitive has no product transaction; native terminal recovery checks stale-generation append rejection."
  , mkCase "stale_append_rejected_after_terminal_recovery"
      (world 5) [.appendRaw 1 5 (raw 100 0 0 5),
        .recoverTerminal 2 10 8 .failed (.message 200) recoveryItems,
        .appendRaw 1 11 lateStaleFlush]
  , mkModelCase "dispatched_tool_wait_explicitly_renews"
      (routedWorld 5) [.acceptRemote, .dispatch 5, .renew 8 10]
      "The abstract nativeCommand child has no native spawn bridge; the same lease-ordering obligation is exercised by a real spawn_subagent counterpart."
  , mkCase "real_spawn_dispatched_wait_explicitly_renews"
      (routedWorld 5) [.realSpawnAccept, .dispatch 5, .renew 8 10] ]

def toolDeadlineFlush (now : Time) : Segment :=
  { ToolDelivery.Cases.toolOutputClose with
    id := 710, close := none, createdAt := now }

/-- Fresh tool output uses the tool's strict deadline, independently of the
parent request lease. Exact replay is checked before the fresh-write deadline
guard and cannot allocate another physical segment. -/
def toolOutputDeadlineCases : List Case :=
  [ mkCase "tool_output_before_deadline"
      (world 5) [.acceptForeground, .dispatch 5,
        .appendToolOutput 1 19 600 (toolDeadlineFlush 19)]
  , mkCase "tool_output_at_deadline"
      (world 5) [.acceptForeground, .dispatch 5,
        .appendToolOutput 1 20 600 (toolDeadlineFlush 20)]
  , mkCase "tool_output_after_deadline"
      (world 5) [.acceptForeground, .dispatch 5,
        .appendToolOutput 1 21 600 (toolDeadlineFlush 21)]
  , mkCase "tool_output_exact_replay_after_deadline"
      (world 5) [.acceptForeground, .dispatch 5,
        .appendToolOutput 1 19 600 (toolDeadlineFlush 19),
        .appendToolOutput 2 21 600 (toolDeadlineFlush 19)] ]

example : toolOutputDeadlineCases.map (fun value => value.expected.map
    (List.map (·.accepted))) =
    [some [true, true, true], some [true, true, true],
     some [true, true, false], some [true, true, true, true]] := by
  native_decide

example : toolOutputDeadlineCases.map (fun value =>
    (value.expected.bind List.getLast?).map
      (fun result => (result.segments.length, result.leaseDeadline))) =
    [some (2, some 10), some (2, some 10),
     some (1, some 10), some (2, some 10)] := by
  native_decide

/-- Four inference sources whose insertion order and JSON spelling both
disagree with numeric `(scope, turn, attempt)` identity order. -/
private def numericRecoveryRawA : Segment :=
  { raw 910 9 0 5 with coordinate := ⟨10, .provider 9 9 9⟩ }

private def numericRecoveryRawB : Segment :=
  { raw 911 10 0 5 with coordinate := ⟨10, .provider 9 9 10⟩ }

private def numericRecoveryRawC : Segment :=
  { raw 912 0 0 5 with coordinate := ⟨10, .provider 9 10 0⟩ }

private def numericRecoveryRawD : Segment :=
  { raw 913 0 0 5 with coordinate := ⟨10, .provider 10 0 0⟩ }

private def numericRecoveryItems : List RecoveryItem :=
  [ ⟨{ partialClose 920 9 1 10 with coordinate := numericRecoveryRawA.coordinate },
      some (recoveryMessage 930 920 0 10)⟩
  , ⟨{ partialClose 921 10 1 10 with coordinate := numericRecoveryRawB.coordinate },
      some (recoveryMessage 931 921 1 10)⟩
  , ⟨{ partialClose 922 0 1 10 with coordinate := numericRecoveryRawC.coordinate },
      some (recoveryMessage 932 922 2 10)⟩
  , ⟨{ partialClose 923 0 1 10 with coordinate := numericRecoveryRawD.coordinate },
      some (recoveryMessage 933 923 3 10)⟩ ]

private def numericRecoveryInputs : List Input :=
  [.appendRaw 1 5 numericRecoveryRawD,
   .appendRaw 1 5 numericRecoveryRawC,
   .appendRaw 1 5 numericRecoveryRawB,
   .appendRaw 1 5 numericRecoveryRawA,
   .recoverTerminal 2 10 8 .failed (.message 933) numericRecoveryItems]

example : ((run (world 5) 600 numericRecoveryInputs).bind List.getLast?).map
    (fun observation => (observation.accepted,
      observation.messages.map (fun message => (message.header.id, message.sequence)))) =
    some (true, [(930, 0), (931, 1), (932, 2), (933, 3)]) := by
  native_decide

/-- Terminal recovery is distinct from resumable generation replacement. The
selection is exact and supplied to the canonical transaction, not chosen by
the observation adapter. -/
def terminalRecoveryCases : List Case :=
  [ mkCase "expired_failure_recovery_closes_prefix_and_selects_header"
      (world 5) [.appendRaw 1 5 (raw 100 0 0 5),
        .recoverTerminal 2 10 8 .failed (.message 200) recoveryItems]
  , mkCase "terminal_recovery_exact_replay_preserves_publication"
      (world 5) [.appendRaw 1 5 (raw 100 0 0 5),
        .recoverTerminal 2 10 8 .failed (.message 200) recoveryItems,
        .recoverTerminal 2 11 8 .failed (.message 200) recoveryItems]
  , mkCase "expired_interrupt_recovery_without_output_selects_none"
      (world 5) [.recoverTerminal 2 10 8 .interrupted .noMessage []]
  , mkCase "terminal_recovery_rejects_invalid_selection"
      (world 5) [.recoverTerminal 2 10 8 .failed (.message 999) []]
  , mkCase "recovery_orders_numeric_scope_turn_and_attempt"
      (world 5) numericRecoveryInputs ]

/-- One native transaction covers the tool close and its paired result header.
The separate close/deliver steps remain executable model witnesses, not claims
about observable native transaction boundaries. -/
def nativeToolCompletionCases : List Case :=
  [ mkCase "foreground_tool_completion_atomically_pairs_result"
      (world 5) [.acceptForeground, .dispatch 5, .completeForeground,
        .terminalizeCompleted] ]

def regressedProviderClose : Segment :=
  { providerTurn with id := 505, flush := none, createdAt := 4 }

def regressedProviderMessage : MessageEnvelope :=
  { providerMessage with
    header := { providerMessage.header with refs := [⟨505, 0⟩, ⟨505, 1⟩] }
    blocks := [.text ⟨⟨505, 0⟩, .full⟩,
      .toolCall 600 "native-call" none "child" ⟨⟨505, 1⟩, .full⟩ none none]
    createdAt := 4 }

/-- Publication integrity and the tool lifecycle beyond the foreground path. -/
def publicationCases : List Case :=
  [ mkCase "short_complete_closure_rejected_after_two_flushes"
      (world 5) [.appendRaw 1 5 providerFirstFlush, .appendRaw 1 5 providerSecondFlush,
        .acceptTurn shortProviderClose shortProviderMessage [remoteAdmission]]
  , mkCase "complete_closure_timestamp_cannot_precede_committed_data"
      (world 5) [.appendRaw 1 5 providerFirstFlush,
        .acceptTurn regressedProviderClose regressedProviderMessage [foregroundAdmission]]
  , mkCase "real_spawn_same_route_replay_is_idempotent"
      (routedWorld 5) [.realSpawnAccept, .realSpawnAccept]
  , mkCase "real_spawn_depth_two_copies_parent_depth"
      (routedDepthWorld 2) [.realSpawnAccept]
  , mkCase "real_spawn_depth_three_copies_parent_depth"
      (routedDepthWorld Subagent.maxSubagentDepth) [.realSpawnAccept]
  , mkCase "real_spawn_fresh_parent_workspace_mismatch_rejected"
      (routedWorld 5) [.realSpawnWorkspaceDrift]
  , mkCase "real_spawn_route_behavior_drift_rejected_on_replay"
      (routedWorld 5) [.realSpawnAccept, .realSpawnBehaviorDrift]
  , mkCase "real_spawn_route_workspace_drift_rejected_on_replay"
      (routedWorld 5) [.realSpawnAccept, .realSpawnWorkspaceDrift]
  , mkModelCase "accepted_remote_behavior_is_immutable_across_replay"
      (routedWorld 5) [.acceptRemote, .acceptRemoteBehaviorDrift]
      "The abstract nativeCommand child cannot enter the spawn_subagent publication owner; real_spawn_route_behavior_drift_rejected_on_replay exercises its immutable route contract natively."
  , mkModelCase "accepted_remote_workspace_source_is_immutable_across_replay"
      (routedWorld 5) [.acceptRemote, .acceptRemoteWorkspaceDrift]
      "The abstract nativeCommand child cannot enter the spawn_subagent publication owner; real_spawn_route_workspace_drift_rejected_on_replay exercises its immutable workspace contract natively."
  , mkModelCase "background_tool_closes_after_parent_terminal"
      (world 5) [.acceptForeground, .dispatch 5, .backgroundTool, .backgroundReceipt,
        .terminalizeCompleted, .closeForeground]
      "The split close after background receipt has no direct native transaction; bind the bridge-specific receipt and terminal callback scenario separately."
  , mkCaseFor "spawned_admission_replays_inertly" (world 5) 601
      [.acceptTurn spawnProviderTurn spawnProviderMessage [foregroundAdmission],
        .dispatch 5, .admitSpawned spawnedAdmission, .dispatchCall 5 601,
        .admitSpawned spawnedAdmission]
  , { mkCaseFor "spawned_admission_conflicting_child_document_rejected" (world 5) 601
      [.acceptTurn spawnProviderTurn spawnProviderMessage [foregroundAdmission],
        .dispatch 5, .admitSpawned spawnedAdmission, .dispatchCall 5 601,
        .admitSpawned spawnedAdmission,
        .admitSpawned { spawnedAdmission with document := 602 }] with
      nativeGap := some "The native spawned-child owner derives the child document from the parent and has no candidate child-document argument; it cannot execute the modeled conflicting-document admission." } ]

example : (run (routedWorld 5) 600 [.realSpawnAccept, .realSpawnAccept]).map
    (List.map (·.accepted)) = some [true, true] := by
  native_decide

/-- A distinct replicated twin makes the source unreconstructable. Revocation
must still terminate the request without discarding either fact. -/
def integrityCases : List Case :=
  [ mkCase "corrupt_twin_revocation_cancels_pending_tool"
      (world 5) [.acceptForeground, .replicate corruptTwin, .revokeDead 1 5 8]
  , mkModelCase "corrupt_twin_revocation_hands_off_running_tool"
      (world 5) [.acceptForeground, .dispatch 5, .replicate corruptTwin,
        .revokeDead 1 5 8, .closeForeground]
      "Native tool completion after request revocation closes and publishes atomically; bind that post-revocation callback scenario separately." ]

/-- The compaction watermark shares the transcript allocator with tool
delivery. The first script pins cursor eligibility. The second pins why a
compacted prefix stays stable: a background receipt is the native pairing row,
so a late result is rejected at every sequence, with or without a cursor,
rather than republished as a second native result inside that prefix. -/
def compactionCases : List Case :=
  [ mkModelCase "compaction_cursor_requires_stable_published_prefix"
      (world 5) [.acceptForeground, .dispatch 5, .compact 0, .closeForeground,
        .deliverForeground, .publishAuthored authored compactionBoundary,
        .compact 3, .compact 2, .compact 2, .compact 1]
      "Native complete_raw_with_presentation cannot expose a close-only prefix to compaction; use an atomic-completion cursor scenario."
  , mkModelCase "late_foreground_result_rejected_after_background_receipt"
      (world 5) [.acceptForeground, .dispatch 5, .backgroundTool, .backgroundReceipt,
        .publishAuthored authored compactionBoundary, .compact 2,
        .closeForeground, .deliverResult 1, .deliverResult 3, .compact 3]
      "The bridge background receipt and late result need a native bridge callback/compaction scenario; split close and delivery are not physical transactions." ]

def auxiliaryCloseCases : List Case :=
  [ mkCase "compaction_auxiliary_complete_close_is_audit_only" (world 5)
      [.appendRaw 1 5 (AuxiliaryCases.observed .compaction),
       .closeAuxiliary 5 7 (AuxiliaryCases.close .compaction .complete)]
  , mkCase "fallback_auxiliary_partial_close_is_audit_only" (world 5)
      [.appendRaw 1 5 (AuxiliaryCases.observed .compactionFallback),
       .closeAuxiliary 5 7 (AuxiliaryCases.close .compactionFallback .«partial»)]
  , mkCase "auxiliary_close_rejects_expired_parent_lease" (world 5)
      [.appendRaw 1 5 (AuxiliaryCases.observed .compaction),
       .closeAuxiliary 11 7
        { (AuxiliaryCases.close .compaction .complete) with createdAt := 11 }] ]
  ++ [mkCase "auxiliary_close_replay_rejects_stale_writer" (world 5)
      [.appendRaw 1 5 (AuxiliaryCases.observed .compaction),
       .closeAuxiliary 5 7 (AuxiliaryCases.close .compaction .complete),
       .closeAuxiliary 5 8 (AuxiliaryCases.staleWriterClose .compaction .complete)]]

example : (auxiliaryCloseCases.map (fun value =>
    value.expected.map (List.map (·.accepted)))) =
    [some [true, true], some [true, true], some [true, false],
      some [true, true, false]] := by native_decide

/-- Write-gate scheduling premise: a suspended same-task holder publishes
nothing. It must be the final step, since a suspended holder cannot release. -/
def schedulingCases : List Case :=
  [ mkCase "suspended_same_task_holder_append_rejected"
      (world 5) [.appendWhileSiblingWaits (raw 100 0 0 5)] ]

def cases : List Case :=
  schedulingCases ++ toolSeamCases ++ nativeToolCompletionCases ++ leaseOrderingCases ++
    toolOutputDeadlineCases ++ terminalRecoveryCases ++
    publicationCases ++ integrityCases ++
    compactionCases ++ auxiliaryCloseCases ++ livePartialCases

def contextFieldsJson (context : ToolExecution.ToolCallContext) : String :=
  "\"call_id\":" ++ toString context.callId ++ ","
    ++ "\"request_id\":" ++ toString context.requestId ++ ","
    ++ "\"state\":" ++ jsonString context.state.toDefraDB ++ ","
    ++ "\"operation\":" ++ jsonString context.operation.toDefraDB ++ ","
    ++ "\"deadline\":" ++ toString context.deadline ++ ","
    ++ "\"started_at\":" ++ jsonOptionalNat context.startedAt ++ ","
    ++ "\"current_time\":" ++ toString context.currentTime ++ ","
    ++ "\"failure_class\":" ++
      (context.failureClass.map (jsonString ∘ ToolExecution.FailureClass.toDefraDB)).getD "null" ++ ","
    ++ "\"persistence\":" ++ jsonString context.persistence.toDefraDB ++ ","
    ++ "\"await_mode\":" ++ jsonString context.awaitMode.toDefraDB ++ ","
    ++ "\"cancel_policy\":" ++ jsonString context.cancelPolicy.toDefraDB ++
      ",\"child_request_id\":" ++ jsonOptionalNat context.childRequestId ++
      ",\"spawn_behavior_id\":" ++ jsonOptionalNat context.spawnBehaviorId

def delegatedWorkspaceJson (value : Option DelegatedWorkspace) : String :=
  (value.map (fun workspace =>
      "{\"workspace_id\":" ++ toString workspace.workspaceId ++
      ",\"workspace_owner_agent_did\":" ++ toString workspace.ownerAgent ++
      ",\"workspace_seal_hash\":" ++ jsonOptionalNat workspace.sealHash ++
      ",\"workspace_authority\":" ++ jsonString workspace.authority.toDefraDB ++ "}")).getD "null"

def admissionJson (value : ToolAdmission) : String :=
  "{" ++ "\"document\":" ++ toString value.document ++ "," ++
    contextFieldsJson value.context ++ ",\"delegated_workspace\":" ++
    delegatedWorkspaceJson value.delegatedWorkspace ++ "}"

def spawnedAdmissionJson (value : SpawnedToolAdmission) : String :=
  "{" ++ "\"document\":" ++ toString value.document ++ ","
    ++ "\"parent_tool_document\":" ++ toString value.parentToolDoc ++ "," ++
    contextFieldsJson value.context ++ "}"

def recoveryItemJson (value : RecoveryItem) : String :=
  "{\"closing\":" ++ canonicalSegmentJson value.closing ++ ",\"message\":" ++
    (value.message.map canonicalMessageJson).getD "null" ++ "}"

def targetJson (value : RemoteTarget) : String :=
  "{\"call\":" ++ toString value.call ++ ",\"coordinator\":" ++
    toString value.coordinator ++ ",\"target\":" ++ toString value.target ++
    ",\"behavior\":" ++ toString value.behavior ++ "}"

def seedJson (value : World) : String :=
  "{" ++ "\"request_id\":" ++ toString value.requestId ++ ","
    ++ "\"session_id\":" ++ toString value.sessionId ++ ","
    ++ "\"principal\":" ++ toString value.principal ++ ","
    ++ "\"subagent_depth\":" ++ toString value.subagentDepth ++ ","
    ++ "\"workspace\":" ++ delegatedWorkspaceJson value.workspace ++ ","
    ++ "\"remote_routes\":" ++ jsonArray (value.remoteRoutes.map fun (call, target, behavior) =>
      "{\"call\":" ++ toString call ++ ",\"target\":" ++ toString target ++
        ",\"behavior\":" ++ toString behavior ++ "}") ++ ","
    ++ "\"lease\":" ++ Conformance.RequestExecutionLeaseContracts.worldJson value.lease ++ ","
    ++ "\"transcript_session_id\":" ++ toString value.transcript.sessionId ++ ","
    ++ "\"next_sequence\":" ++ toString value.transcript.nextSeq ++ ","
    ++ "\"segments\":[],\"messages\":[],\"tool_calls\":[],\"in_flight\":[]}"

def inputJson (input : Input) : String :=
  let common := "{\"operation\":" ++ jsonString input.tag ++
    ",\"actor\":" ++ toString input.actor ++ ",\"now\":" ++ toString input.now
  match input.step with
  | .replicate record => common ++ ",\"record\":" ++ canonicalSegmentJson record ++ "}"
  | .commitWhileSiblingWaits (.append generation record) =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"record\":" ++
        canonicalSegmentJson record ++ "}"
  | .commitWhileSiblingWaits _ => "null"
  | .commit operation =>
  match operation with
  | .accept generation closing message targets admissions =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"closing\":" ++
        canonicalSegmentJson closing ++ ",\"message\":" ++ canonicalMessageJson message ++
        ",\"targets\":" ++ jsonArray (targets.map targetJson) ++ ",\"admissions\":" ++
        jsonArray (admissions.map admissionJson) ++ "}"
  | .dispatch generation permit =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"call\":" ++
        toString permit.call ++ ",\"cancellation_allows\":" ++
        jsonOptionalBool (some permit.cancellationAllows) ++ ",\"tool_policy_allows\":" ++
        jsonOptionalBool (some permit.toolPolicyAllows) ++ "}"
  | .toolClose document (.native .complete) record =>
      common ++ ",\"document\":" ++ toString document ++
        ",\"authority_outcome\":\"complete\",\"record\":" ++
        canonicalSegmentJson record ++ "}"
  | .toolComplete document (.native .complete) record message =>
      common ++ ",\"document\":" ++ toString document ++
        ",\"authority_outcome\":\"complete\",\"record\":" ++
        canonicalSegmentJson record ++ ",\"message\":" ++
        canonicalMessageJson message ++ "}"
  | .toolDeliver document message =>
      common ++ ",\"document\":" ++ toString document ++ ",\"message\":" ++
        canonicalMessageJson message ++ "}"
  | .terminalize generation outcome (.message id) =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"outcome\":" ++
        jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName outcome) ++
        ",\"selection\":{\"kind\":\"message\",\"id\":" ++
        toString id ++ "}}"
  | .recover expected fresh duration deadline items =>
      common ++ ",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++ ",\"duration\":" ++
        toString duration ++ ",\"deadline\":" ++ toString deadline ++
        ",\"items\":" ++ jsonArray (items.map recoveryItemJson) ++ "}"
  | .recoverTerminal expected fresh outcome selection items =>
      let selected := match selection with
        | .noMessage => "{\"kind\":\"no_message\"}"
        | .message id => "{\"kind\":\"message\",\"id\":" ++ toString id ++ "}"
      common ++ ",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++
        ",\"outcome\":" ++
          jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName outcome) ++
        ",\"selection\":" ++ selected ++
        ",\"items\":" ++ jsonArray (items.map recoveryItemJson) ++ "}"
  | .closePartial generation item =>
      common ++ ",\"generation\":" ++ toString generation ++
        ",\"item\":" ++ recoveryItemJson item ++ "}"
  | .renew generation expectedDeadline =>
      common ++ ",\"generation\":" ++ toString generation ++
        ",\"expected_deadline\":" ++ toString expectedDeadline ++ "}"
  | .append generation record =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"record\":" ++
        canonicalSegmentJson record ++ "}"
  | .closeAuxiliary generation closing =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"closing\":" ++
        canonicalSegmentJson closing ++ "}"
  | .toolAppend document record =>
      common ++ ",\"document\":" ++ toString document ++ ",\"record\":" ++
        canonicalSegmentJson record ++ "}"
  | .toolControl generation document .background =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"document\":" ++
        toString document ++ ",\"action\":\"background\"}"
  | .backgroundReceipt parentDocument closing message =>
      common ++ ",\"parent_document\":" ++ toString parentDocument ++ ",\"closing\":" ++
        canonicalSegmentJson closing ++ ",\"message\":" ++ canonicalMessageJson message ++ "}"
  | .admitSpawned generation admission =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"admission\":" ++
        spawnedAdmissionJson admission ++ "}"
  | .authored generation closing message =>
      common ++ ",\"generation\":" ++ toString generation ++ ",\"closing\":" ++
        canonicalSegmentJson closing ++ ",\"message\":" ++ canonicalMessageJson message ++ "}"
  | .compact cursor => common ++ ",\"cursor\":" ++ toString cursor ++ "}"
  | .revoke expected fresh outcome (.message id) =>
      common ++ ",\"expected_generation\":" ++ toString expected ++
        ",\"fresh_generation\":" ++ toString fresh ++ ",\"outcome\":" ++
        jsonString (Conformance.RequestExecutionLeaseContracts.outcomeName outcome) ++
        ",\"selection\":{\"kind\":\"message\",\"id\":" ++ toString id ++ "}}"
  | _ => "null"

def observationJson (value : Observation) : String :=
  "{" ++ "\"accepted\":" ++ jsonOptionalBool (some value.accepted) ++ ","
    ++ "\"generation\":" ++ jsonOptionalNat value.generation ++ ","
    ++ "\"terminal_generation\":" ++ jsonOptionalNat value.terminalGeneration ++ ","
    ++ "\"request_state\":" ++ jsonString value.requestState ++ ","
    ++ "\"tool_state\":" ++ (value.toolState.map jsonString).getD "null" ++ ","
    ++ "\"tool_stuck_since\":" ++ jsonOptionalNat value.toolStuckSince ++ ","
    ++ "\"tool_cancel_intent_at\":" ++ jsonOptionalNat value.toolCancelIntentAt ++ ","
    ++ "\"in_flight\":" ++ jsonOptionalBool (some value.inFlight) ++ ","
    ++ "\"next_sequence\":" ++ toString value.nextSequence ++ ","
    ++ "\"accepted_sequence\":" ++ jsonOptionalNat value.acceptedSequence ++ ","
    ++ "\"physical_tool_request\":" ++ jsonOptionalNat value.physicalToolRequest ++ ","
    ++ "\"lease_deadline\":" ++ jsonOptionalNat value.leaseDeadline ++ ","
    ++ "\"compaction_cursor\":" ++ jsonOptionalNat value.compactionCursor ++ ","
    ++ "\"segments\":" ++ jsonArray (value.segments.map canonicalSegmentJson) ++ ","
    ++ "\"messages\":" ++ jsonArray (value.messages.map canonicalMessageJson) ++ "}"

def caseJson (value : Case) : String :=
  "{" ++ "\"kind\":" ++ jsonString
    (if value.nativeGap.isSome then "model_execution" else "native_execution") ++ ","
    ++ "\"name\":" ++ jsonString value.name ++ ","
    ++ "\"native_gap\":" ++ (value.nativeGap.map jsonString).getD "null" ++ ","
    ++ "\"seed\":" ++ seedJson value.seed ++ ","
    ++ "\"query_document\":" ++ toString value.queryDocument ++ ","
    ++ "\"operations\":" ++ jsonArray (value.inputs.map inputJson) ++ ","
    ++ "\"expected_observations\":" ++
      (value.expected.map (jsonArray ∘ List.map observationJson)).getD "null" ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : cases.all (fun value => value.expected.isSome) = true := by native_decide

/-- Collections represented as empty, plus omitted optional execution state, are
empty in every modeled seed. -/
example : cases.all (fun value => value.seed.segments.isEmpty && value.seed.messages.isEmpty &&
    value.seed.transcript.messages.isEmpty && value.seed.transcript.toolCalls.isEmpty &&
    value.seed.transcript.inFlight == ∅ && value.seed.compactionCursor.isNone &&
    value.seed.toolContexts.isEmpty && value.seed.delegatedCalls.isEmpty &&
    value.seed.terminalSelection.isNone && value.seed.gateOwner.isNone &&
    value.seed.claimed.isNone) = true := by
  native_decide

/-- Every script is substantive, serializable through its modeled operations,
and produces exactly one observation for each attempted operation. -/
example : cases.all (fun value => !value.inputs.isEmpty &&
    (value.inputs.map inputJson).all (· != "null") &&
    match value.expected with
    | some observations => observations.length == value.inputs.length
    | none => false) = true := by
  native_decide

/-- Cardinality alone is insufficient: exact normalized durable facts distinguish
same-count payload and identity corruption. -/
example :
    let original := raw 100 0 0 5
    let changedPayload := { original with flush := original.flush.map fun flush =>
      { flush with payload := [66] } }
    let changedIdentity := { original with id := 101 }
    let originalMessage := providerMessage
    let changedMessagePayload := { originalMessage with blocks := [] }
    let changedMessageIdentity :=
      { originalMessage with header := { originalMessage.header with id := 999 } }
    [original].length = [changedPayload].length ∧
      observe 600 true (world 5 [original]) ≠ observe 600 true (world 5 [changedPayload]) ∧
      observe 600 true (world 5 [original]) ≠ observe 600 true (world 5 [changedIdentity]) ∧
      [originalMessage].length = [changedMessagePayload].length ∧
      observe 600 true { world 5 with messages := [originalMessage] } ≠
        observe 600 true { world 5 with messages := [changedMessagePayload] } ∧
      observe 600 true { world 5 with messages := [originalMessage] } ≠
        observe 600 true { world 5 with messages := [changedMessageIdentity] } := by
  native_decide

end Conformance.NativeExecutionContracts
