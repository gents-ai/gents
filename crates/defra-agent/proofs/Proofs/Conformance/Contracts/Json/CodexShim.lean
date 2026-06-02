import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.CodexShim.LocalInterrupt

/-!
# Codex Shim Contract JSON

Finite adapter projection vectors for the stock Codex app-server surface
exposed by the DEFRA Codex shim.
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
  effectivelyTerminal : Bool
  interruptibleRequestState : Bool

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
    ++ "\"terminal\":" ++ boolString witness.terminal ++ ","
    ++ "\"effectively_terminal\":"
      ++ boolString witness.effectivelyTerminal ++ ","
    ++ "\"interruptible_request_state\":"
      ++ boolString witness.interruptibleRequestState
    ++ "}"

def codexShimProjectionCases : List CodexShimProjectionCase :=
  [ { witness := "codex_shim.projection.pending_no_response"
    , leanTheorems :=
        [ "CodexShim.project_pending_is_in_progress"
        , "CodexShim.nonterminal_without_response_projects_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "pending"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    , effectivelyTerminal := false
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.claimed_no_response"
    , leanTheorems :=
        [ "CodexShim.project_claimed_is_in_progress"
        , "CodexShim.nonterminal_without_response_projects_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "claimed"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    , effectivelyTerminal := false
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.processing_streaming_response"
    , leanTheorems :=
        [ "CodexShim.project_processing_is_in_progress"
        , "CodexShim.request_transition_projection_monotonic"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "processing"
    , responseStatus := some "streaming"
    , localInterruptAcked := false
    , projectedPhase := "inProgress"
    , terminal := false
    , effectivelyTerminal := false
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.nonterminal_complete_response"
    , leanTheorems :=
        [ "CodexShim.response_complete_advances_nonterminal_to_completed"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "processing"
    , responseStatus := some "complete"
    , localInterruptAcked := false
    , projectedPhase := "completed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.nonterminal_error_response"
    , leanTheorems :=
        [ "CodexShim.response_error_advances_nonterminal_to_failed"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "processing"
    , responseStatus := some "error"
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.completed_request"
    , leanTheorems :=
        [ "CodexShim.project_completed_is_completed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "completed"
    , responseStatus := some "error"
    , localInterruptAcked := false
    , projectedPhase := "completed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.failed_request"
    , leanTheorems :=
        [ "CodexShim.project_failed_is_failed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "failed"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.dead_request"
    , leanTheorems :=
        [ "CodexShim.project_dead_is_failed"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "dead"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "failed"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.superseded_request"
    , leanTheorems :=
        [ "CodexShim.project_superseded_is_interrupted"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "superseded"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.interrupted_request"
    , leanTheorems :=
        [ "CodexShim.project_interrupted_is_interrupted"
        , "CodexShim.terminal_request_overrides_response"
        , "CodexShim.terminal_request_projects_terminal"
        , "CodexShim.codex_turn_terminates_precisely"
        ]
    , requestState := "interrupted"
    , responseStatus := none
    , localInterruptAcked := false
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := false
    }
  , { witness := "codex_shim.projection.local_interrupt_preempts_core_state"
    , leanTheorems :=
        [ "CodexShim.local_interrupt_projects_interrupted"
        , "CodexShim.local_interrupt_never_projects_in_progress"
        , "CodexShim.codex_turn_terminates_precisely"
        , "CodexShim.local_interrupt_requires_interruptible"
        , "CodexShim.local_interrupt_shortcut_sound"
        ]
    , requestState := "processing"
    , responseStatus := some "streaming"
    , localInterruptAcked := true
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  , { witness := "codex_shim.projection.local_interrupt_input_required"
    , leanTheorems :=
        [ "CodexShim.local_interrupt_projects_interrupted"
        , "CodexShim.local_interrupt_never_projects_in_progress"
        , "CodexShim.codex_turn_terminates_precisely"
        , "CodexShim.local_interrupt_requires_interruptible"
        , "CodexShim.local_interrupt_shortcut_sound"
        ]
    , requestState := "inputRequired"
    , responseStatus := none
    , localInterruptAcked := true
    , projectedPhase := "interrupted"
    , terminal := true
    , effectivelyTerminal := true
    , interruptibleRequestState := true
    }
  ]

def codexShimProjectionCasesJson : String :=
  jsonArray (codexShimProjectionCases.map codexShimProjectionCaseJson)

structure CodexShimTurnLifecycleCase where
  witness : String
  leanTheorems : List String
  action : String
  prePhase : String
  postPhase : String
  preLexOrd : Nat
  postLexOrd : Nat
  monotonic : Bool

def codexShimTurnLifecycleCaseJson
    (witness : CodexShimTurnLifecycleCase) : String :=
  "{"
    ++ "\"witness\":" ++ jsonString witness.witness ++ ","
    ++ "\"lean_theorems\":" ++ jsonStringArray witness.leanTheorems ++ ","
    ++ "\"action\":" ++ jsonString witness.action ++ ","
    ++ "\"pre_phase\":" ++ jsonString witness.prePhase ++ ","
    ++ "\"post_phase\":" ++ jsonString witness.postPhase ++ ","
    ++ "\"pre_lex_ord\":" ++ toString witness.preLexOrd ++ ","
    ++ "\"post_lex_ord\":" ++ toString witness.postLexOrd ++ ","
    ++ "\"monotonic\":" ++ boolString witness.monotonic
    ++ "}"

def codexShimTurnLifecycleCases : List CodexShimTurnLifecycleCase :=
  [ { witness := "codex_shim.turn_lifecycle.start"
    , leanTheorems := [ "CodexShim.turn_lifecycle_never_regresses" ]
    , action := "start"
    , prePhase := "notStarted"
    , postPhase := "inProgress"
    , preLexOrd := 0
    , postLexOrd := 1
    , monotonic := true
    }
  , { witness := "codex_shim.turn_lifecycle.complete"
    , leanTheorems := [ "CodexShim.turn_lifecycle_never_regresses" ]
    , action := "complete"
    , prePhase := "inProgress"
    , postPhase := "completed"
    , preLexOrd := 1
    , postLexOrd := 2
    , monotonic := true
    }
  , { witness := "codex_shim.turn_lifecycle.fail"
    , leanTheorems := [ "CodexShim.turn_lifecycle_never_regresses" ]
    , action := "fail"
    , prePhase := "inProgress"
    , postPhase := "failed"
    , preLexOrd := 1
    , postLexOrd := 2
    , monotonic := true
    }
  , { witness := "codex_shim.turn_lifecycle.interrupt"
    , leanTheorems :=
        [ "CodexShim.turn_lifecycle_never_regresses"
        , "CodexShim.interrupt_from_in_progress_is_terminal"
        , "CodexShim.interrupt_step_is_terminal"
        ]
    , action := "interrupt"
    , prePhase := "inProgress"
    , postPhase := "interrupted"
    , preLexOrd := 1
    , postLexOrd := 2
    , monotonic := true
    }
  ]

def codexShimTurnLifecycleCasesJson : String :=
  jsonArray (codexShimTurnLifecycleCases.map codexShimTurnLifecycleCaseJson)

end Conformance.Contracts
