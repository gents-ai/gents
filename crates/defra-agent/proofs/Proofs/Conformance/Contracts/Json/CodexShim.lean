import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases
import Proofs.CodexShim.Projection

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

end Conformance.Contracts
