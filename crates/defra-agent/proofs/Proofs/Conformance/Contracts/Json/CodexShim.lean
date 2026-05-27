import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.CodexShim.Projection
import Proofs.CodexShim.Steering

/-!
# Codex Shim Contract JSON

Finite adapter vectors for the stock Codex app-server surface exposed by the
DEFRA Codex shim.
-/

namespace Conformance.Contracts

open Conformance.ContractCases

structure CodexShimProjectionCase where
  witness : String
  leanTheorems : List String
  requestState : String
  responseStatus : Option String
  localInterruptAcked : Bool
  projectedPhase : String
  terminal : Bool

def codexShimProjectionCaseJson (witness : CodexShimProjectionCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"request_state\":" ++ jsonString witness.requestState ++ ","
    ++ "\"response_status\":"
      ++ jsonOptionalString witness.responseStatus ++ ","
    ++ "\"local_interrupt_acked\":"
      ++ boolString witness.localInterruptAcked ++ ","
    ++ "\"projected_phase\":" ++ jsonString witness.projectedPhase ++ ","
    ++ "\"terminal\":" ++ boolString witness.terminal
    ++ "}"

def codexShimProjectionCases : List CodexShimProjectionCase :=
  [ { witness := "codex_shim.projection.pending_no_response"
    , leanTheorems :=
        [ "CodexShim.project_pending_is_in_progress"
        , "CodexShim.nonterminal_without_response_projects_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        ]
    , requestState := "pending"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    }
  , { witness := "codex_shim.projection.claimed_no_response"
    , leanTheorems :=
        [ "CodexShim.project_claimed_is_in_progress"
        , "CodexShim.nonterminal_without_response_projects_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        ]
    , requestState := "claimed"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    }
  , { witness := "codex_shim.projection.processing_streaming_response"
    , leanTheorems :=
        [ "CodexShim.project_processing_is_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        ]
    , requestState := "processing"
    , responseStatus := some "streaming"
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    }
  , { witness := "codex_shim.projection.nonterminal_complete_response"
    , leanTheorems :=
        [ "CodexShim.response_complete_advances_nonterminal_to_completed" ]
    , requestState := "processing"
    , responseStatus := some "complete"
    , localInterruptAcked := false
    , projectedPhase := "completed"
    , terminal := true
    }
  , { witness := "codex_shim.projection.nonterminal_error_response"
    , leanTheorems :=
        [ "CodexShim.response_error_advances_nonterminal_to_failed" ]
    , requestState := "processing"
    , responseStatus := some "error"
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    }
  , { witness := "codex_shim.projection.completed_request"
    , leanTheorems :=
        [ "CodexShim.project_completed_is_completed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        ]
    , requestState := "completed"
    , responseStatus := some "error"
    , localInterruptAcked := false
    , projectedPhase := "completed"
    , terminal := true
    }
  , { witness := "codex_shim.projection.failed_request"
    , leanTheorems :=
        [ "CodexShim.project_failed_is_failed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        ]
    , requestState := "failed"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    }
  , { witness := "codex_shim.projection.dead_request"
    , leanTheorems :=
        [ "CodexShim.project_dead_is_failed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        ]
    , requestState := "dead"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    }
  , { witness := "codex_shim.projection.superseded_request"
    , leanTheorems :=
        [ "CodexShim.project_superseded_is_interrupted"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        ]
    , requestState := "superseded"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "interrupted"
    , terminal := true
    }
  , { witness := "codex_shim.projection.interrupted_request"
    , leanTheorems :=
        [ "CodexShim.project_interrupted_is_interrupted"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        ]
    , requestState := "interrupted"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "interrupted"
    , terminal := true
    }
  , { witness := "codex_shim.projection.local_interrupt_preempts_core_state"
    , leanTheorems :=
        [ "CodexShim.local_interrupt_projects_interrupted"
        , "CodexShim.local_interrupt_never_projects_in_progress"
        ]
    , requestState := "processing"
    , responseStatus := some "streaming"
    , localInterruptAcked := true
    , projectedPhase := "interrupted"
    , terminal := true
    }
  ]

def codexShimProjectionCasesJson : String :=
  jsonArray (codexShimProjectionCases.map codexShimProjectionCaseJson)

structure CodexShimSteeringCase where
  witness : String
  leanTheorems : List String
  activeTurnId : String
  expectedTurnId : String
  activeRequestId : String
  emitsTurnStarted : Bool
  emitsTurnCompleted : Bool
  preservesActiveTurn : Bool
  clearsActiveTurn : Bool
  terminalStatus : Option String
  committedUserMessageDelta : Nat
  queueSource : Option String
  queuePolicy : Option String
  queuedAfterRequestId : Option String
  forwardsRequestInterrupt : Bool
  requiresRequestTransitionBeforeAck : Bool
  requestTransition : Option String
  requestFrom : Option String
  requestTo : Option String

def codexShimSteeringCaseJson (witness : CodexShimSteeringCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"active_turn_id\":" ++ jsonString witness.activeTurnId ++ ","
    ++ "\"expected_turn_id\":" ++ jsonString witness.expectedTurnId ++ ","
    ++ "\"active_request_id\":" ++ jsonString witness.activeRequestId ++ ","
    ++ "\"emits_turn_started\":" ++ boolString witness.emitsTurnStarted ++ ","
    ++ "\"emits_turn_completed\":"
      ++ boolString witness.emitsTurnCompleted ++ ","
    ++ "\"preserves_active_turn\":" ++ boolString witness.preservesActiveTurn ++ ","
    ++ "\"clears_active_turn\":" ++ boolString witness.clearsActiveTurn ++ ","
    ++ "\"terminal_status\":"
      ++ jsonOptionalString witness.terminalStatus ++ ","
    ++ "\"committed_user_message_delta\":"
      ++ toString witness.committedUserMessageDelta ++ ","
    ++ "\"queue_source\":" ++ jsonOptionalString witness.queueSource ++ ","
    ++ "\"queue_policy\":" ++ jsonOptionalString witness.queuePolicy ++ ","
    ++ "\"queued_after_request_id\":"
      ++ jsonOptionalString witness.queuedAfterRequestId ++ ","
    ++ "\"forwards_request_interrupt\":"
      ++ boolString witness.forwardsRequestInterrupt ++ ","
    ++ "\"requires_request_transition_before_ack\":"
      ++ boolString witness.requiresRequestTransitionBeforeAck ++ ","
    ++ "\"request_transition\":"
      ++ jsonOptionalString witness.requestTransition ++ ","
    ++ "\"request_from\":" ++ jsonOptionalString witness.requestFrom ++ ","
    ++ "\"request_to\":" ++ jsonOptionalString witness.requestTo
    ++ "}"

def codexShimAcceptedSteerCase : CodexShimSteeringCase :=
  { witness := "codex_shim.turn_steer.accepted_same_turn"
  , leanTheorems :=
      [ "CodexShim.accept_steer_preserves_active_turn"
      , "CodexShim.accept_steer_does_not_emit_turn_started"
      , "CodexShim.accept_steer_appends_steering_entry"
      , "CodexShim.accept_steer_records_queued_request"
      ]
  , activeTurnId := "turn-active"
  , expectedTurnId := "turn-active"
  , activeRequestId := "request-active"
  , emitsTurnStarted := false
  , emitsTurnCompleted := false
  , preservesActiveTurn := true
  , clearsActiveTurn := false
  , terminalStatus := none
  , committedUserMessageDelta := 1
  , queueSource := some "steering"
  , queuePolicy := some "append"
  , queuedAfterRequestId := some "request-active"
  , forwardsRequestInterrupt := false
  , requiresRequestTransitionBeforeAck := false
  , requestTransition := none
  , requestFrom := none
  , requestTo := none
  }

def codexShimDrainSteeringCase : CodexShimSteeringCase :=
  { witness := "codex_shim.turn_steer.drain_queued_request"
  , leanTheorems :=
      [ "CodexShim.drain_steering_advances_active_request_without_completing_turn"
      , "CodexShim.drain_steering_uses_completed_projection"
      ]
  , activeTurnId := "turn-active"
  , expectedTurnId := "turn-active"
  , activeRequestId := "request-active"
  , emitsTurnStarted := false
  , emitsTurnCompleted := false
  , preservesActiveTurn := true
  , clearsActiveTurn := false
  , terminalStatus := none
  , committedUserMessageDelta := 0
  , queueSource := none
  , queuePolicy := none
  , queuedAfterRequestId := none
  , forwardsRequestInterrupt := false
  , requiresRequestTransitionBeforeAck := false
  , requestTransition := some "drain_steering"
  , requestFrom := some "projectedCompleted"
  , requestTo := some "inProgress"
  }

def codexShimInterruptCase : CodexShimSteeringCase :=
  { witness := "codex_shim.turn_interrupt.local_terminal"
  , leanTheorems :=
      [ "CodexShim.interrupt_active_clears_active_turn"
      , "CodexShim.interrupt_active_emits_terminal_turn"
      , "CodexShim.interrupt_active_does_not_wait_for_request_transition"
      , "CodexShim.interrupt_active_does_not_preserve_active_turn"
      , "CodexShim.interrupt_cannot_stutter"
      ]
  , activeTurnId := "turn-active"
  , expectedTurnId := "turn-active"
  , activeRequestId := "request-active"
  , emitsTurnStarted := false
  , emitsTurnCompleted := true
  , preservesActiveTurn := false
  , clearsActiveTurn := true
  , terminalStatus := some "interrupted"
  , committedUserMessageDelta := 0
  , queueSource := none
  , queuePolicy := none
  , queuedAfterRequestId := none
  , forwardsRequestInterrupt := true
  , requiresRequestTransitionBeforeAck := false
  , requestTransition := none
  , requestFrom := none
  , requestTo := none
  }

def codexShimSteeringCases : List CodexShimSteeringCase :=
  [ codexShimAcceptedSteerCase
  , codexShimDrainSteeringCase
  , codexShimInterruptCase
  ]

def codexShimSteeringCasesJson : String :=
  jsonArray (codexShimSteeringCases.map codexShimSteeringCaseJson)

end Conformance.Contracts
