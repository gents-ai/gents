import Proofs.StreamingResponse.Transition

/-!
# StreamingResponse Conformance Vectors

Finite witness rows for Rust conformance tests. Each row pins one
transition's expected pre/post shape and the runtime call site it
corresponds to.
-/

namespace StreamingResponse

structure ResponseTransitionCase where
  name                       : String
  group                      : String
  action                     : String
  legal                      : Bool
  preStatus                  : String
  postStatus                 : String
  preLiveTail                : String
  postLiveTail               : String
  preTokenCount              : Nat
  postTokenCount             : Nat
  errorReason                : Option String
  preMaterializedSeq         : Option Transcript.Sequence
  postMaterializedSeq        : Option Transcript.Sequence
  expectedRequestState       : Option String
  expectedRequestPersistence : Option String
  deriving Repr

def beginEmitsStreamingEmpty : ResponseTransitionCase :=
  { name := "begin_emits_streaming_empty"
  , group := "normal"
  , action := "begin"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "streaming"
  , preLiveTail := "empty"
  , postLiveTail := "empty"
  , preTokenCount := 0
  , postTokenCount := 0
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := none
  , expectedRequestPersistence := none
  }

def writeTokensAdvancesProgress : ResponseTransitionCase :=
  { name := "write_tokens_advances_progress"
  , group := "normal"
  , action := "write_tokens"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "streaming"
  , preLiveTail := "empty"
  , postLiveTail := "nonEmpty"
  , preTokenCount := 0
  , postTokenCount := 5
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := none
  , expectedRequestPersistence := none
  }

def writeReasoningNoTokenBump : ResponseTransitionCase :=
  { name := "write_reasoning_no_token_bump"
  , group := "normal"
  , action := "write_reasoning"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "streaming"
  , preLiveTail := "empty"
  , postLiveTail := "nonEmpty"
  , preTokenCount := 0
  , postTokenCount := 0
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := none
  , expectedRequestPersistence := none
  }

def flushPendingIsAbstractNoop : ResponseTransitionCase :=
  { name := "flush_pending_is_abstract_noop"
  , group := "normal"
  , action := "flush"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "streaming"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "nonEmpty"
  , preTokenCount := 3
  , postTokenCount := 3
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := none
  , expectedRequestPersistence := none
  }

def resetTailClearsButPreservesTokens : ResponseTransitionCase :=
  { name := "reset_tail_clears_but_preserves_tokens"
  , group := "normal"
  , action := "reset_tail"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "streaming"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "empty"
  , preTokenCount := 7
  , postTokenCount := 7
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := none
  , expectedRequestPersistence := none
  }

def finalizeCompleteClearsAndMaterializes : ResponseTransitionCase :=
  { name := "finalize_complete_clears_and_materializes"
  , group := "normal"
  , action := "finalize_complete"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "complete"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "empty"
  , preTokenCount := 10
  , postTokenCount := 10
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := some 42
  , expectedRequestState := some "completed"
  , expectedRequestPersistence := some "committed"
  }

def finalizeErrorInferenceFailedClears : ResponseTransitionCase :=
  { name := "finalize_error_inference_failed_clears"
  , group := "normal"
  , action := "finalize_error"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "error"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "empty"
  , preTokenCount := 8
  , postTokenCount := 8
  , errorReason := some "inferenceFailed"
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := some "failed"
  , expectedRequestPersistence := some "committed"
  }

def finalizeErrorIdleTimeoutRequiresDeadline : ResponseTransitionCase :=
  { name := "finalize_error_idle_timeout_requires_deadline"
  , group := "normal"
  , action := "finalize_error"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "error"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "empty"
  , preTokenCount := 4
  , postTokenCount := 4
  , errorReason := some "streamIdleTimeout"
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := some "failed"
  , expectedRequestPersistence := some "committed"
  }

def recoverInterruptedKeepsContent : ResponseTransitionCase :=
  { name := "recover_interrupted_keeps_content"
  , group := "recovery"
  , action := "recover_interrupted"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "error"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "nonEmpty"
  , preTokenCount := 6
  , postTokenCount := 6
  , errorReason := some "daemonRestartRecovery"
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := some "failed"
  , expectedRequestPersistence := some "committed"
  }

def observeIdempotentFinalizeIsNoop : ResponseTransitionCase :=
  { name := "observe_idempotent_finalize_is_noop"
  , group := "idempotent"
  , action := "observe_idempotent_finalize"
  , legal := true
  , preStatus := "complete"
  , postStatus := "complete"
  , preLiveTail := "empty"
  , postLiveTail := "empty"
  , preTokenCount := 12
  , postTokenCount := 12
  , errorReason := none
  , preMaterializedSeq := some 99
  , postMaterializedSeq := some 99
  , expectedRequestState := none
  , expectedRequestPersistence := none
  }

def setInterruptedAtDoesNotChangeStatus : ResponseTransitionCase :=
  { name := "set_interrupted_at_does_not_change_status"
  , group := "boundary"
  , action := "set_interrupted_at"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "streaming"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "nonEmpty"
  , preTokenCount := 2
  , postTokenCount := 2
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := none
  , expectedRequestState := none
  , expectedRequestPersistence := none
  }

def bridgeCompletedPairsRequestCommitted : ResponseTransitionCase :=
  { name := "bridge_completed_pairs_request_committed"
  , group := "bridge"
  , action := "finalize_complete"
  , legal := true
  , preStatus := "streaming"
  , postStatus := "complete"
  , preLiveTail := "nonEmpty"
  , postLiveTail := "empty"
  , preTokenCount := 15
  , postTokenCount := 15
  , errorReason := none
  , preMaterializedSeq := none
  , postMaterializedSeq := some 88
  , expectedRequestState := some "completed"
  , expectedRequestPersistence := some "committed"
  }

def responseTransitionCases : List ResponseTransitionCase :=
  [ beginEmitsStreamingEmpty
  , writeTokensAdvancesProgress
  , writeReasoningNoTokenBump
  , flushPendingIsAbstractNoop
  , resetTailClearsButPreservesTokens
  , finalizeCompleteClearsAndMaterializes
  , finalizeErrorInferenceFailedClears
  , finalizeErrorIdleTimeoutRequiresDeadline
  , recoverInterruptedKeepsContent
  , observeIdempotentFinalizeIsNoop
  , setInterruptedAtDoesNotChangeStatus
  , bridgeCompletedPairsRequestCommitted
  ]

end StreamingResponse
