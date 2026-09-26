import Proofs.CanonicalOutput.Execution.Properties

namespace CanonicalOutput.Execution.Examples

open CanonicalOutput RequestExecutionLease

def lease (now : Time := 5) : RequestExecutionLease.World Generation :=
  { request := .processing, lease := .active 7 5 10, usedGenerations := [7]
    now := now, continuationRequired := false,
    tokenChargeRequired := false, continuationCount := 0, tokenChargeCount := 0 }

def transcript : Transcript.TranscriptState :=
  { sessionId := 1, nextSeq := 0, messages := [], toolCalls := [], inFlight := ∅ }

def world (now : Time := 5) (segments : List Segment := []) : World :=
  { requestId := 10, sessionId := 1, purpose := .normal, principal := 1, remoteRoutes := [],
    lease := lease now, segments := segments,
    messages := [], transcript := transcript, delegatedCalls := [], terminalSelection := none }

def raw (id attempt ordinal : Nat) (createdAt : Time) : Segment :=
  { id := id, coordinate := ⟨10, .provider 0 0 attempt⟩, writer := .request 7,
    flush := some ⟨ordinal,
      [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩], [65]⟩,
    close := none, createdAt := createdAt }

def emptyClose (id attempt : Nat) (outcome : Outcome) (createdAt : Time) : Segment :=
  { id := id, coordinate := ⟨10, .provider 0 0 attempt⟩, writer := .request 7,
    flush := none, close := some (.closed outcome 0 []), createdAt := createdAt }

def emptyAssistant (id : Nat) (createdAt : Time) : MessageEnvelope :=
  { header :=
      { id := id, session := 1, request := some 10, origin := none
        refs := [], outcome := .complete, role := .assistant
        publication := .requestExecution 7 }
    key := s!"assistant-{id}", sequence := 0, nativeId := none, blocks := []
    createdAt := createdAt }

def succeeds {error value : Type} : Except error value → Bool
  | .ok _ => true
  | .error _ => false

theorem guarded_raw_append_is_reachable :
    succeeds (appendRaw (world 5) 7 (raw 100 0 0 5)) = true := by
  native_decide

def conflictedFutureOutput : World :=
  { world 8 with segments :=
      [raw 100 0 0 5,
       { raw 100 0 0 99 with flush := some ⟨0,
          [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩], [66]⟩ }] }

/-- Canonical conflicts and future-dated records require their own integrity
handling, but cannot prevent the current owner from winning a due renewal CAS. -/
theorem explicit_renewal_ignores_conflicted_future_output :
    succeeds (renew conflictedFutureOutput 7 10) = true := by
  native_decide

def rawAppendKeepsDeadline : Bool :=
  match appendRaw (world 5) 7 (raw 100 0 0 5) with
  | .error _ => false
  | .ok post => post.lease.lease == .active 7 5 10

theorem producer_output_does_not_renew_deadline :
    rawAppendKeepsDeadline = true := by native_decide

theorem payload_free_assistant_can_commit_zero_stream_source :
    succeeds (acceptAndPublish (world 5) 7 (emptyClose 100 0 .complete 5)
      (emptyAssistant 200 5) [] []) = true := by
  native_decide

theorem complete_closure_replay_is_not_a_retry_retraction :
    (match retractBeforeRetry (world 6 [emptyClose 100 0 .complete 5]) 7
        (emptyClose 100 0 .complete 5) with
      | .error .invalidSegment => true | _ => false) = true := by
  native_decide

theorem payload_free_assistant_can_commit_header_only :
    succeeds (publishHeaderOnly (world 5) 7 (emptyAssistant 200 5) []) = true := by
  native_decide

def authored : Segment :=
  { id := 300, coordinate := ⟨10, .authored 0⟩, writer := .request 7,
    flush := some ⟨0, [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩], [65]⟩,
    close := some (.closed .complete 1 [1]), createdAt := 5 }

def authoredMessage : MessageEnvelope :=
  { header :=
      { id := 301, session := 1, request := some 10, origin := none
        refs := [⟨300, 0⟩], outcome := .complete, role := .user
        publication := .requestExecution 7 }
    key := "user-0", sequence := 0, nativeId := none
    blocks := [.text ⟨⟨300, 0⟩, .full⟩]
    createdAt := 5 }

theorem authored_payload_closure_and_header_commit_together :
    succeeds (publishAuthored (world 5) 7 authored authoredMessage) = true := by
  native_decide

def extraAuthoredFlush : Segment :=
  { id := 302, coordinate := ⟨10, .authored 0⟩, writer := .request 7
    flush := some ⟨1,
      [⟨0, 1, none⟩], [66]⟩
    close := none, createdAt := 5 }

def firstAuthoredFlush : Segment := { authored with close := none }

def shortAuthoredClose : Segment :=
  { authored with id := 303, flush := none, close := some (.closed .complete 1 [1]) }

def shortAuthoredMessage : MessageEnvelope :=
  { header := { authoredMessage.header with refs := [⟨303, 0⟩] }
    key := authoredMessage.key, sequence := authoredMessage.sequence
    nativeId := authoredMessage.nativeId
    blocks := [.text ⟨⟨303, 0⟩, .full⟩], createdAt := authoredMessage.createdAt }

theorem fresh_authored_close_must_cover_every_committed_flush :
    succeeds (publishAuthored (world 5 [firstAuthoredFlush, extraAuthoredFlush]) 7
      shortAuthoredClose shortAuthoredMessage) = false := by
  native_decide

theorem short_close_was_reconstructable_but_not_authoritative :
    validateClosingRecord
        [firstAuthoredFlush, extraAuthoredFlush, shortAuthoredClose]
        shortAuthoredClose = true ∧
      freshCompleteExtentExact
        [firstAuthoredFlush, extraAuthoredFlush, shortAuthoredClose]
        shortAuthoredClose = false := by
  native_decide

def systemAuthored : Segment :=
  { authored with id := 310, coordinate := ⟨10, .authored 1⟩ }

def systemMessage : MessageEnvelope :=
  { header :=
      { id := 311, session := 1, request := some 10, origin := none
        refs := [⟨310, 0⟩], outcome := .complete, role := .system
        publication := .requestExecution 7 }
    key := "system-0", sequence := 0, nativeId := none
    blocks := [.text ⟨⟨310, 0⟩, .full⟩]
    createdAt := 5 }

def sequencedUserMessage : MessageEnvelope :=
  { authoredMessage with key := "user-1", sequence := 1 }

def systemThenUserPublishes : Bool :=
  match publishAuthored (world 5) 7 systemAuthored systemMessage with
  | .error _ => false
  | .ok afterSystem => match publishAuthored afterSystem 7 authored sequencedUserMessage with
    | .error _ => false
    | .ok afterUser =>
        afterUser.transcript.nextSeq == 2 &&
          systemMessage ∈ afterUser.messages && sequencedUserMessage ∈ afterUser.messages

theorem system_reserves_sequence_before_next_user :
    systemThenUserPublishes = true := by native_decide

def partialClose (id attempt count : Nat) (createdAt : Time) : Segment :=
  { id := id, coordinate := ⟨10, .provider 0 0 attempt⟩, writer := .request 7,
    flush := none, close := some (.closed .«partial» count (if count = 0 then [] else [1])),
    createdAt := createdAt }

def recoveryMessage (id closeId sequence : Nat) (createdAt : Time) : MessageEnvelope :=
  { header :=
      { id := id, session := 1, request := some 10, origin := none
        refs := [⟨closeId, 0⟩], outcome := .«partial», role := .assistant
        publication := .requestRecovery 8 }
    key := s!"recovery-{id}", sequence := sequence, nativeId := none
    blocks := [.text ⟨⟨closeId, 0⟩, .full⟩]
    createdAt := createdAt }

def silentWorld : World :=
  world 20 [raw 100 0 0 1, raw 110 1 0 2]

def recoveryItems : List RecoveryItem :=
  [⟨partialClose 101 0 1 20, some (recoveryMessage 201 101 0 20)⟩,
   ⟨partialClose 111 1 1 20, some (recoveryMessage 211 111 1 20)⟩]

theorem expired_batch_closes_every_open_provider_source :
    succeeds (recoverExpiredBatch silentWorld 7 8 5 30 recoveryItems) = true := by
  native_decide

def reusedWorld : World :=
  world 30 [raw 100 0 0 1, partialClose 101 0 1 20]

theorem exact_existing_partial_closure_is_reused :
    succeeds (recoverExpiredBatch reusedWorld 7 8 5 40
      [⟨partialClose 101 0 1 20, some (recoveryMessage 201 101 0 30)⟩]) = true := by
  native_decide

def recoveredHeaderfulWorld : World :=
  match recoverExpiredBatch reusedWorld 7 8 5 40
      [⟨partialClose 101 0 1 20, some (recoveryMessage 201 101 0 30)⟩] with
  | .ok recovered => recovered
  | .error _ => reusedWorld

def headerfulRecoveryReplayIsIdentity : Bool :=
  match recoverExpiredBatch recoveredHeaderfulWorld 7 8 5 40
      [⟨partialClose 101 0 1 20, some (recoveryMessage 201 101 0 30)⟩] with
  | .error _ => false
  | .ok post => post == recoveredHeaderfulWorld

theorem headerful_recovery_exact_replay_is_identity :
    headerfulRecoveryReplayIsIdentity = true := by
  native_decide

def wrongFreshRecoveryMessage : MessageEnvelope :=
  { recoveryMessage 201 101 0 30 with header :=
      { (recoveryMessage 201 101 0 30).header with publication := .requestRecovery 9 } }

theorem recovery_replay_rejects_mutated_fresh_generation_binding :
    succeeds (recoverExpiredBatch recoveredHeaderfulWorld 7 8 5 40
      [⟨partialClose 101 0 1 20, some wrongFreshRecoveryMessage⟩]) = false := by
  native_decide

theorem recovery_replay_rejects_mutated_closure_coordinate :
    succeeds (recoverExpiredBatch recoveredHeaderfulWorld 7 8 5 40
      [⟨{ partialClose 101 0 1 20 with coordinate := ⟨10, .provider 0 0 9⟩ },
        some (recoveryMessage 201 101 0 30)⟩]) = false := by
  native_decide

def foreignHeaderlessPartial : Segment :=
  { id := 190, coordinate := ⟨999, .authored 0⟩, writer := .request 7
    flush := none, close := some (.closed .«partial» 0 []), createdAt := 20 }

def recoveredWithForeignPartial : World :=
  { recoveredHeaderfulWorld with
    segments := recoveredHeaderfulWorld.segments ++ [foreignHeaderlessPartial] }

theorem recovery_replay_rejects_foreign_headerless_partial_artifact :
    succeeds (recoverExpiredBatch recoveredWithForeignPartial 7 8 5 40
      [⟨foreignHeaderlessPartial, none⟩]) = false := by
  native_decide

def nonzeroTextRaw : Segment :=
  { raw 120 2 0 3 with flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 1, kind := .text }⟩], [65]⟩ }

def nonzeroTextWorld : World := world 20 [nonzeroTextRaw]

theorem nonzero_text_part_recovers_without_a_header :
    succeeds (recoverExpiredBatch nonzeroTextWorld 7 8 5 30
      [⟨partialClose 121 2 1 20, none⟩]) = true := by
  native_decide

def incompleteMediaDeclaration : Declaration :=
  { block := 1, part := 0, kind := .media, mediaKind := some .image }

def incompleteNonTextRaw : Segment :=
  { id := 130, coordinate := ⟨10, .provider 0 0 3⟩, writer := .request 7
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .reasoning }⟩,
       ⟨1, 1, some incompleteMediaDeclaration⟩], [65, 66]⟩
    close := none, createdAt := 3 }

def incompleteNonTextClose : Segment :=
  { id := 131, coordinate := ⟨10, .provider 0 0 3⟩, writer := .request 7
    flush := none, close := some (.closed .«partial» 1 [1, 1]), createdAt := 20 }

theorem incomplete_reasoning_and_media_recover_without_relabeling :
    succeeds (recoverExpiredBatch (world 20 [incompleteNonTextRaw]) 7 8 5 30
      [⟨incompleteNonTextClose, none⟩]) = true := by
  native_decide

def duplicateUnsupportedPositionIsMalformed : Bool :=
  match recoveryText
      [(0, { block := 0, part := 1, kind := .text }),
       (1, { block := 0, part := 1, kind := .reasoning })] with
  | .error .malformedRuns => true
  | _ => false

theorem duplicate_native_position_stays_malformed_even_when_text_part_is_unsupported :
    duplicateUnsupportedPositionIsMalformed = true := by
  native_decide

def providerTurn : Segment :=
  { id := 500, coordinate := ⟨10, .provider 0 1 0⟩, writer := .request 7
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩,
       ⟨1, 2, some { block := 1, part := 0, kind := .arguments, tool := some ⟨"native-call", none, "child"⟩ }⟩],
      [65, 123, 125]⟩
    close := some (.closed .complete 1 [1, 2]), createdAt := 5 }

def providerFirstFlush : Segment := { providerTurn with close := none }

def providerSecondFlush : Segment :=
  { id := 502, coordinate := providerTurn.coordinate, writer := providerTurn.writer
    flush := some ⟨1, [⟨0, 1, none⟩, ⟨1, 1, none⟩], [66, 32]⟩
    close := none, createdAt := 5 }

def shortProviderClose : Segment :=
  { providerTurn with id := 503, flush := none }

def providerMessage : MessageEnvelope :=
  { header :=
      { id := 501, session := 1, request := some 10, origin := none
        refs := [⟨500, 0⟩, ⟨500, 1⟩], outcome := .complete, role := .assistant
        publication := .requestExecution 7 }
    key := "provider-1", sequence := 0, nativeId := some "native-message"
    blocks := [.text ⟨⟨500, 0⟩, .full⟩,
      .toolCall 600 "native-call" none "child" ⟨⟨500, 1⟩, .full⟩ none none]
    createdAt := 5 }

def shortProviderMessage : MessageEnvelope :=
  { header := { providerMessage.header with refs := [⟨503, 0⟩, ⟨503, 1⟩] }
    key := providerMessage.key, sequence := providerMessage.sequence, nativeId := providerMessage.nativeId
    blocks := [.text ⟨⟨503, 0⟩, .full⟩,
      .toolCall 600 "native-call" none "child" ⟨⟨503, 1⟩, .full⟩ none none]
    createdAt := 5 }

theorem fresh_provider_close_must_cover_every_committed_flush :
    succeeds (acceptAndPublish
      { world 5 with segments := [providerFirstFlush, providerSecondFlush] }
      7 shortProviderClose shortProviderMessage [] []) = false := by
  native_decide

def remote : RemoteTarget := ⟨600, 1, 2, 8⟩
def permit : DispatchPermit := ⟨600, true, true⟩
def remoteToolContext : ToolExecution.ToolCallContext :=
  { callId := 600, requestId := 10, state := .pending
    operation := .nativeCommand, deadline := 20, currentTime := 5
    persistence := .committed, awaitMode := .background, childRequestId := some 50,
    spawnBehaviorId := some 8 }
def remoteWorkspace : DelegatedWorkspace :=
  ⟨70, 2, some 71, .readOnly⟩
def remoteAdmission : ToolAdmission := ⟨600, remoteToolContext, some remoteWorkspace⟩
def foregroundToolContext : ToolExecution.ToolCallContext :=
  { remoteToolContext with awaitMode := .foreground, childRequestId := none, spawnBehaviorId := none }
def foregroundAdmission : ToolAdmission := ⟨600, foregroundToolContext, none⟩
def routedWorld (now : Time := 5) : World :=
  { world now with remoteRoutes := [(600, 2, 8)], workspace := some remoteWorkspace }

def routedDepthWorld (depth : Nat) : World :=
  { routedWorld 5 with subagentDepth := depth }

/-- The actual provider-native spawn argument stream used by conformance. -/
def realSpawnArguments : String :=
  "{\"await_mode\":\"background\",\"name\":\"lean-behavior-8\",\"prompt\":\"work\"}"

def realSpawnArgumentBytes : List UInt8 := realSpawnArguments.toUTF8.data.toList

def realSpawnProviderTurn : Segment :=
  { providerTurn with
    flush := some ⟨0,
      [⟨0, realSpawnArgumentBytes.length, some
        { block := 0, part := 0, kind := .arguments,
          tool := some ⟨"native-call", none, "spawn_subagent"⟩ }⟩],
      realSpawnArgumentBytes⟩
    close := some (.closed .complete 1 [realSpawnArgumentBytes.length]) }

def realSpawnProviderTurnForArguments (arguments : String) : Segment :=
  let bytes := arguments.toUTF8.data.toList
  { realSpawnProviderTurn with
    flush := some ⟨0,
      [⟨0, bytes.length, some
        { block := 0, part := 0, kind := .arguments,
          tool := some ⟨"native-call", none, "spawn_subagent"⟩ }⟩], bytes⟩
    close := some (.closed .complete 1 [bytes.length]) }

def realSpawnProviderMessage : MessageEnvelope :=
  { providerMessage with
    header := { providerMessage.header with refs := [⟨500, 0⟩] }
    blocks := [.toolCall 600 "native-call" none "spawn_subagent"
      ⟨⟨500, 0⟩, .full⟩ none none] }

/-- Distinct physical calls and supplied argument bytes use the same
accept-and-publish owner. The flush and close lengths are derived from those
bytes; the addressed host resolves the requested workspace later. -/
def acceptedDelegatedCallFor (call depth : Nat)
    (workspace : Option DelegatedWorkspace)
    (arguments : String := realSpawnArguments) : Option DelegatedCall := do
  let world := { routedDepthWorld depth with
    remoteRoutes := [(call, 2, 8)], workspace := workspace }
  let toolContext := { remoteToolContext with callId := call }
  let admission : ToolAdmission := ⟨call, toolContext, workspace⟩
  let message := { realSpawnProviderMessage with
    blocks := [.toolCall call "native-call" none "spawn_subagent"
      ⟨⟨500, 0⟩, .full⟩ none none] }
  let accepted ← (acceptAndPublish world 7
    (realSpawnProviderTurnForArguments arguments) message
    [{ remote with call := call }] [admission]).toOption
  accepted.delegatedCalls.find? (fun row => row.call == call)

def acceptedDelegatedCallAtDepthWithWorkspace (depth : Nat)
    (workspace : DelegatedWorkspace) : Option DelegatedCall := do
  acceptedDelegatedCallFor 600 depth (some workspace)

def acceptedDelegatedCallAtDepth (depth : Nat) : Option DelegatedCall :=
  acceptedDelegatedCallAtDepthWithWorkspace depth remoteWorkspace

theorem accepted_depth_two_creates_child_at_bound :
    (acceptedDelegatedCallAtDepth 2).bind
      (receiveDelegatedChildDepth 1 2 8) = some Subagent.maxSubagentDepth := by
  native_decide

theorem accepted_depth_three_cannot_create_child :
    (acceptedDelegatedCallAtDepth Subagent.maxSubagentDepth).bind
      (receiveDelegatedChildDepth 1 2 8) = none := by
  native_decide

def observedParentWorkspace : Workspace.ObservedWorkspace :=
  ⟨remoteWorkspace.workspaceId, remoteWorkspace.ownerAgent,
    remoteWorkspace.sealHash, .ready, true⟩

def observedProvisionedWorkspace : Workspace.ObservedWorkspace :=
  ⟨71, 2, none, .ready, true⟩

theorem readonly_parent_inheritance_cannot_escalate :
    (acceptedDelegatedCallAtDepth 2).bind (fun row =>
      receiveDelegatedChild 1 2 8 1 2 row
        (.inherit observedParentWorkspace)) =
      some (3, some remoteWorkspace) := by
  native_decide

theorem provisioned_child_can_have_distinct_identity_without_escalation :
    (acceptedDelegatedCallAtDepth 2).bind (fun row =>
      receiveDelegatedChild 1 2 8 1 2 row
        (.provision observedParentWorkspace true (some observedProvisionedWorkspace))) =
      some (3, some ⟨71, 2, none, .readOnly⟩) := by
  native_decide

theorem unverified_provision_is_not_a_child_workspace :
    (acceptedDelegatedCallAtDepth 2).bind (fun row =>
      receiveDelegatedChild 1 2 8 1 2 row
        (.provision observedParentWorkspace true
          (some { observedProvisionedWorkspace with available := false }))) = none := by
  native_decide

theorem failed_provision_cannot_stamp_child :
    (acceptedDelegatedCallAtDepth 2).bind (fun row =>
      receiveDelegatedChild 1 2 8 1 2 row
        (.provision observedParentWorkspace true none)) = none := by
  native_decide

theorem changed_parent_seal_blocks_provision :
    (acceptedDelegatedCallAtDepth 2).bind (fun row =>
      receiveDelegatedChild 1 2 8 1 2 row
        (.provision { observedParentWorkspace with sealHash := some 99 } true
          (some observedProvisionedWorkspace))) = none := by
  native_decide

theorem absent_to_present_parent_seal_blocks_provision :
    (acceptedDelegatedCallAtDepthWithWorkspace 2
      { remoteWorkspace with sealHash := none }).bind (fun row =>
      receiveDelegatedChild 1 2 8 1 2 row
        (.provision observedParentWorkspace true
          (some observedProvisionedWorkspace))) = none := by
  native_decide

theorem accepted_depth_replay_rejects_changed_source :
    (match acceptAndPublish (routedDepthWorld 2) 7 realSpawnProviderTurn realSpawnProviderMessage
        [remote] [remoteAdmission] with
    | .error _ => false
    | .ok accepted =>
      let altered := { accepted with subagentDepth := 3 }
      match acceptAndPublish altered 7 realSpawnProviderTurn realSpawnProviderMessage [remote]
          [remoteAdmission] with
      | .error .identityCollision => true
      | _ => false) = true := by
  native_decide

def acceptedAndDispatched : Bool :=
  match acceptAndPublish (routedWorld 5) 7 providerTurn providerMessage [remote]
      [remoteAdmission] with
  | .error _ => false
  | .ok accepted => match dispatch accepted 7 permit with
    | .error _ => false
    | .ok dispatched =>
        physicalRunning dispatched 600 && !(600 ∈ dispatched.transcript.inFlight) &&
          dispatched.delegatedCalls.any (fun row =>
            row.call == 600 && row.coordinator == 1 && row.target == 2 &&
              row.input.arguments == "{}")

theorem nonempty_provider_acceptance_delegates_then_dispatches :
    acceptedAndDispatched = true := by native_decide

theorem expired_acceptance_is_rejected :
    (match acceptAndPublish (routedWorld 10) 7
        { providerTurn with createdAt := 10 }
        { providerMessage with createdAt := 10 } [remote] [remoteAdmission] with
      | .error .leaseRejected => true | _ => false) = true := by
  native_decide

def foregroundAcceptedAndDispatched : Except Error World := do
  let accepted ← acceptAndPublish (world 5) 7 providerTurn providerMessage []
    [foregroundAdmission]
  dispatch accepted 7 permit

theorem foreground_provider_call_reaches_physical_running_state :
    (match foregroundAcceptedAndDispatched with
      | .ok post => physicalRunning post 600 && 600 ∈ post.transcript.inFlight
      | .error _ => false) = true := by
  native_decide

def acceptedToolWorld : World :=
  match acceptAndPublish (routedWorld 5) 7 providerTurn providerMessage [remote]
      [remoteAdmission] with
  | .ok accepted => accepted
  | .error _ => world 5

def acceptedThenTerminalized : Bool :=
  match terminalize acceptedToolWorld 7 .completed (.message 501) with
  | .error _ => false
  | .ok terminal =>
      terminal.transcript.toolCalls.any (fun row =>
        row.callId == 600 && row.state == .cancelled) &&
      providerMessage ∈ terminal.messages

theorem terminalization_fails_owned_pending_call_and_keeps_header :
    acceptedThenTerminalized = true := by
  native_decide

def acceptedToTerminalTrace : Bool :=
  match acceptAndPublish (routedWorld 5) 7 providerTurn providerMessage [remote]
      [remoteAdmission] with
  | .error _ => false
  | .ok accepted => match terminalize accepted 7 .completed (.message 501) with
    | .error _ => false
    | .ok terminal => terminal.transcript.toolCalls.any (fun row =>
        row.callId == 600 && row.state == .cancelled)

theorem accepted_to_terminal_trace_exists : acceptedToTerminalTrace = true := by
  native_decide

def expiredToolWorld : World :=
  { acceptedToolWorld with lease :=
      { acceptedToolWorld.lease with now := effectiveExpiry acceptedToolWorld.lease } }

theorem expired_dispatch_is_rejected :
    (match dispatch expiredToolWorld 7 permit with
      | .error .leaseRejected => true | _ => false) = true := by
  native_decide

def forgedTranscriptOnlyWorld : World :=
  { acceptedToolWorld with segments := [], messages := [] }

theorem transcript_rows_without_canonical_publication_cannot_dispatch :
    (match dispatch forgedTranscriptOnlyWorld 7 permit with
      | .error .publicationIncomplete => true | _ => false) = true := by
  native_decide

def missingDelegatedRowWorld : World :=
  { acceptedToolWorld with delegatedCalls := [] }

theorem remote_call_without_exact_delegated_row_cannot_dispatch :
    (match dispatch missingDelegatedRowWorld 7 permit with
      | .error .publicationIncomplete => true | _ => false) = true := by
  native_decide

theorem missing_remote_route_projection_is_rejected :
    (match acceptAndPublish (routedWorld 5) 7 providerTurn providerMessage []
        [remoteAdmission] with
      | .error .invalidDelegation => true | _ => false) = true := by
  native_decide

theorem wrong_remote_target_is_rejected :
    (match acceptAndPublish (routedWorld 5) 7 providerTurn providerMessage
        [⟨600, 1, 3, 8⟩] [remoteAdmission] with
      | .error .invalidDelegation => true | _ => false) = true := by
  native_decide

theorem wrong_remote_behavior_is_rejected :
    (match acceptAndPublish (routedWorld 5) 7 providerTurn providerMessage
        [⟨600, 1, 2, 9⟩] [remoteAdmission] with
      | .error .invalidDelegation => true | _ => false) = true := by
  native_decide

def driftedRemoteAdmission : ToolAdmission :=
  ⟨600, { remoteToolContext with spawnBehaviorId := some 9 }, some remoteWorkspace⟩

def driftedWorkspaceAdmission : ToolAdmission :=
  ⟨600, remoteToolContext, some { remoteWorkspace with authority := .readWrite }⟩

theorem fresh_remote_admission_cannot_escalate_parent_stamp :
    (match acceptAndPublish (routedWorld 5) 7 providerTurn providerMessage [remote]
        [driftedWorkspaceAdmission] with
    | .error .transcriptRejected => true
    | _ => false) = true := by
  native_decide

theorem unbound_parent_can_admit_unbound_remote_call :
    (match acceptAndPublish { routedWorld 5 with workspace := none } 7
        providerTurn providerMessage [remote]
        [{ remoteAdmission with delegatedWorkspace := none }] with
    | .ok accepted => accepted.delegatedCalls.length == 1
    | _ => false) = true := by
  native_decide

theorem accepted_call_replay_cannot_select_different_behavior :
    (match acceptAndPublish acceptedToolWorld 7 providerTurn providerMessage [remote]
        [driftedRemoteAdmission] with
      | .error .identityCollision => true | _ => false) = true := by
  native_decide

theorem accepted_call_replay_cannot_change_workspace_source :
    acceptedAdmissionsPresent acceptedToolWorld [driftedWorkspaceAdmission] = false := by
  native_decide

def partialProviderTurn : Segment :=
  { providerTurn with close := some (.closed .«partial» 1 [1, 2]) }

def forgedPartialAcceptedWorld : World :=
  { world 5 with
    segments := [partialProviderTurn]
    messages := [providerMessage]
    transcript := transcript.publishAcceptedAssistant 501 (messageTurn providerMessage) }

theorem partial_closure_cannot_replay_as_complete_acceptance :
    succeeds (acceptAndPublish forgedPartialAcceptedWorld 7 partialProviderTurn
      providerMessage [] [remoteAdmission]) = false := by
  native_decide

def partialAuthored : Segment :=
  { authored with close := some (.closed .«partial» 1 [1]) }

def forgedPartialAuthoredWorld : World :=
  { world 5 with
    segments := [partialAuthored]
    messages := [authoredMessage]
    transcript := transcript.appendUserMessage 301 .ordinary }

theorem partial_closure_cannot_replay_as_complete_authored :
    (match publishAuthored forgedPartialAuthoredWorld 7 partialAuthored authoredMessage with
      | .error .publicationIncomplete => true | _ => false) = true := by
  native_decide

def existingPublishedPartialWorld : World :=
  { reusedWorld with
    messages := [recoveryMessage 201 101 0 30]
    transcript := transcript.publishPartialAssistant 201
      (messageTurn (recoveryMessage 201 101 0 30)) }

theorem published_partial_source_is_not_recovered_again :
    recoverableCoordinates existingPublishedPartialWorld 7 = [] := by
  native_decide

def duplicateHeaderOnlyWorld : World :=
  { world 5 with messages := [emptyAssistant 200 5, emptyAssistant 200 5] }

theorem exact_duplicate_terminal_envelopes_are_idempotent :
    terminalSelectionValid duplicateHeaderOnlyWorld (.message 200) = true := by
  native_decide

end CanonicalOutput.Execution.Examples
