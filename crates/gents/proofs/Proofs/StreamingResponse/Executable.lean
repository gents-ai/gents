import Proofs.StreamingResponse.Transition
import Proofs.InferenceCall.Executable

namespace StreamingResponse

structure ResponseTransitionCase where
  name                       : String
  group                      : String
  action                     : String
  legal                      : Bool
  /-- Action arguments are independent of the expected post-state. -/
  tokenDelta                 : Option Nat
  inputErrorReason           : Option String
  materializeSequence        : Option Transcript.Sequence
  preStatus                  : String
  postStatus                 : String
  preLiveTail                : String
  postLiveTail               : String
  preTailReasoning           : String := "empty"
  postTailReasoning          : String := "empty"
  preDurableReasoning        : String := "empty"
  postDurableReasoning       : String := "empty"
  preTokenCount              : Nat
  postTokenCount             : Nat
  errorReason                : Option String
  preMaterializedSeq         : Option Transcript.Sequence
  postMaterializedSeq        : Option Transcript.Sequence
  expectedRequestState       : Option String
  expectedRequestPersistence : Option String
  deriving Repr

structure ResponseInterruptFlowCase where
  name                         : String
  group                        : String
  action                       : String
  preRequestState              : String
  postRequestState             : String
  preResponseStatus            : String
  postResponseStatus           : String
  preInferenceCallState        : String
  postInferenceCallState       : String
  responseErrorReason          : String
  interruptedAtRequired        : Bool
  completedAtRequired          : Bool
  liveTailCleared              : Bool
  partialTurnMaterialized      : Bool
  requestTerminal              : Bool
  responseTerminal             : Bool
  inferenceCallTerminal        : Bool
  deriving Repr

/-- Conformance rows project typed owner transitions; expected state is never
maintained as an independent table of strings. -/
private structure Sample where
  name : String
  group : String
  action : String
  pre : ResponseContext
  post : ResponseContext
  legal : Transition pre post
  paired : Bool
  requestOutcome : RequestState
  pairedLegal : paired = true → BridgeTransition
    ⟨pre, .processing, .uncommitted⟩ ⟨post, requestOutcome, .committed⟩
  tokenDelta : Option Nat
  inputErrorReason : Option ErrorReason
  materializeSequence : Option Transcript.Sequence

private def unpaired (name group action : String) (pre post : ResponseContext)
    (legal : Transition pre post) : Sample :=
  ⟨name, group, action, pre, post, legal, false, .processing, (by intro h; cases h), none, none, none⟩

private def paired (name group action : String) (pre post : ResponseContext)
    (outcome : RequestState) (legal : Transition pre post)
    (bridge : BridgeTransition ⟨pre, .processing, .uncommitted⟩ ⟨post, outcome, .committed⟩) : Sample :=
  ⟨name, group, action, pre, post, legal, true, outcome, (fun _ => bridge), none, none, none⟩

private def base (tokens : Nat := 0) (tail : LiveTail := .empty) : ResponseContext :=
  { docId := 1, requestId := 1, status := .streaming, liveTail := tail
  , tailReasoning := .empty, durableReasoning := .empty, tokenCount := tokens
  , lastProgressAt := 0, streamIdleDeadline := 10, now := 11
  , errorReason := none, materializedMessageSequence := none, interruptedAt := none }

private def samples : List Sample :=
  [ unpaired "begin_emits_streaming_empty" "normal" "begin" (base) (base)
      (Transition.begin rfl rfl rfl rfl rfl)
  , let delta := 5
    { (unpaired "write_tokens_advances_progress" "normal" "write_tokens" (base)
        { base with liveTail := .nonEmpty, tokenCount := delta, lastProgressAt := 11 }
        (Transition.writeTokens (delta := delta) rfl (by decide) rfl)) with
      tokenDelta := some delta }
  , unpaired "write_reasoning_no_token_bump" "normal" "write_reasoning" (base)
      { base with liveTail := .nonEmpty, tailReasoning := .nonEmpty, lastProgressAt := 11 }
      (Transition.writeReasoning rfl rfl)
  , unpaired "flush_pending_is_abstract_noop" "normal" "flush" (base 3 .nonEmpty) (base 3 .nonEmpty)
      (Transition.flushPending rfl rfl)
  , unpaired "reset_tail_clears_but_preserves_tokens" "normal" "reset_tail" (base 7 .nonEmpty)
      { base 7 .nonEmpty with liveTail := .empty, tailReasoning := .empty }
      (Transition.resetTail rfl rfl)
  , let seq := 42
    { (paired "finalize_complete_clears_and_materializes" "normal" "finalize_complete"
        { base 10 .nonEmpty with tailReasoning := .nonEmpty }
        { base 10 with status := .completed, tailReasoning := .nonEmpty, durableReasoning := .nonEmpty, materializedMessageSequence := some seq } .completed
        (Transition.finalizeComplete rfl rfl)
        (BridgeTransition.finalizeComplete rfl rfl rfl rfl rfl)) with
      materializeSequence := some seq }
  , let reason := ErrorReason.inferenceFailed
    { (paired "finalize_error_inference_failed_clears" "normal" "finalize_error" (base 8 .nonEmpty)
        { base 8 with status := .error, errorReason := some reason } .failed
        (Transition.finalizeError (reason := reason) rfl (by decide) (by decide) rfl)
        (BridgeTransition.finalizeError (reason := reason) rfl (by decide) (by decide) rfl rfl rfl rfl)) with
      inputErrorReason := some reason }
  , let reason := ErrorReason.streamIdleTimeout
    { (paired "finalize_error_idle_timeout_requires_deadline" "normal" "finalize_error" (base 4 .nonEmpty)
        { base 4 with status := .error, errorReason := some reason } .failed
        (Transition.finalizeError (reason := reason) rfl (by decide) (by decide) rfl)
        (BridgeTransition.finalizeError (reason := reason) rfl (by decide) (by decide) rfl rfl rfl rfl)) with
      inputErrorReason := some reason }
  , paired "recover_interrupted_keeps_content" "recovery" "recover_interrupted" (base 6 .nonEmpty)
      { base 6 .nonEmpty with status := .error, errorReason := some .daemonRestartRecovery } .failed
      (Transition.recoverInterrupted rfl rfl)
      (BridgeTransition.recoverPaired rfl rfl rfl rfl rfl)
  , unpaired "observe_idempotent_finalize_is_noop" "idempotent" "observe_idempotent_finalize"
      { base 12 with status := .completed, materializedMessageSequence := some 99 }
      { base 12 with status := .completed, materializedMessageSequence := some 99 }
      (Transition.observeIdempotentFinalize (Or.inl rfl) rfl)
  , unpaired "set_interrupted_at_does_not_change_status" "boundary" "set_interrupted_at" (base 2 .nonEmpty)
      { base 2 .nonEmpty with interruptedAt := some 11 }
      (Transition.setInterruptedAt rfl rfl rfl)
  , let seq := 88
    { (paired "bridge_completed_pairs_request_committed" "bridge" "finalize_complete"
        { base 15 .nonEmpty with tailReasoning := .nonEmpty }
        { base 15 with status := .completed, tailReasoning := .nonEmpty, durableReasoning := .nonEmpty, materializedMessageSequence := some seq } .completed
        (Transition.finalizeComplete rfl rfl)
        (BridgeTransition.finalizeComplete rfl rfl rfl rfl rfl)) with
      materializeSequence := some seq } ]

private def sampleCase (s : Sample) : ResponseTransitionCase :=
  { name := s.name, group := s.group, action := s.action, legal := true
  , tokenDelta := s.tokenDelta, materializeSequence := s.materializeSequence
  , inputErrorReason := s.inputErrorReason.map ErrorReason.toContract
  , preStatus := s.pre.status.toDefraDB, postStatus := s.post.status.toDefraDB
  , preLiveTail := s.pre.liveTail.toContract, postLiveTail := s.post.liveTail.toContract
  , preTailReasoning := s.pre.tailReasoning.toContract, postTailReasoning := s.post.tailReasoning.toContract
  , preDurableReasoning := s.pre.durableReasoning.toContract, postDurableReasoning := s.post.durableReasoning.toContract
  , preTokenCount := s.pre.tokenCount, postTokenCount := s.post.tokenCount
  , errorReason := s.post.errorReason.map ErrorReason.toContract
  , preMaterializedSeq := s.pre.materializedMessageSequence
  , postMaterializedSeq := s.post.materializedMessageSequence
  , expectedRequestState := if s.paired then some s.requestOutcome.toDefraDB else none
  , expectedRequestPersistence := if s.paired then some PersistenceState.committed.toDefraDB else none }

def responseTransitionCases : List ResponseTransitionCase := samples.map sampleCase

/-- The excerpt starts after the existing transcript owner materializes the partial
turn; response interruption preserves that observed handle. This model does not
manufacture transcript writes. Completion timestamp persistence remains an explicit
adapter obligation, outside ResponseContext's projection. -/
private def interruptPre : ResponseContext :=
  { base 2 .nonEmpty with materializedMessageSequence := some 1 }
private def interruptStamped : ResponseContext :=
  { interruptPre with interruptedAt := some 11 }
private def interruptPost : ResponseContext :=
  { interruptStamped with status := .error, liveTail := .empty, errorReason := some .interrupted }
private def interruptCall : InferenceCall :=
  { callId := 1, requestId := 1, backend := ⟨"backend"⟩, state := .running }

example : Transition interruptPre interruptStamped ∧
    BridgeTransition ⟨interruptStamped, .processing, .uncommitted⟩ ⟨interruptPost, .interrupted, .committed⟩ ∧
    InferenceCall.Transition interruptCall interruptCall.cancel :=
  ⟨Transition.setInterruptedAt rfl rfl rfl,
   BridgeTransition.finalizeError (reason := .interrupted) rfl (by decide) (by decide) rfl rfl rfl rfl,
   InferenceCall.cancel_during_stream_transition rfl rfl⟩

def daemonInterruptTerminalizesResponseAndRequest : ResponseInterruptFlowCase :=
  { name := "daemon_interrupt_terminalizes_response_and_request", group := "interrupt", action := "daemon_interrupt_flow"
  , preRequestState := RequestState.processing.toDefraDB, postRequestState := RequestState.interrupted.toDefraDB
  , preResponseStatus := interruptPre.status.toDefraDB, postResponseStatus := interruptPost.status.toDefraDB
  , preInferenceCallState := interruptCall.state.toDefraDB, postInferenceCallState := interruptCall.cancel.state.toDefraDB
  , responseErrorReason := ErrorReason.interrupted.toContract
  , interruptedAtRequired := interruptPost.interruptedAt.isSome
  , completedAtRequired := true
  , liveTailCleared := interruptPost.liveTail == .empty
  , partialTurnMaterialized := interruptPost.materializedMessageSequence.isSome
  , requestTerminal := decide (isTerminal RequestState.interrupted)
  , responseTerminal := decide (isTerminal interruptPost.status)
  , inferenceCallTerminal := decide (isTerminal interruptCall.cancel.state) }

def responseInterruptFlowCases : List ResponseInterruptFlowCase :=
  [daemonInterruptTerminalizesResponseAndRequest]

end StreamingResponse
