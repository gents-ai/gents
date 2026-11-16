import Proofs.Session.ToolExecution.Executable
import Proofs.Session.PromptAssembly.ToolArgumentFailure
import Proofs.Shared.CompletionRetry.InvalidToolProgress

/-!
# A call whose arguments do not parse

The owned loop never runs a tool on arguments it cannot parse. It answers the
call with a notice naming the failure kind, records the started call as failed
with `argumentInvalid`, and charges the invalid-call allowance; one such call
does not end the request, so the model can correct it on its next turn.

Rust owners: `gents_loop::tool_call_lifecycle::ToolOutcome::from_dispatch`
(the notice), the session hook (the durable terminal) and
`gents_loop::loop_stream::invalid_tool_progress` (the allowance).
-/

namespace ToolExecution.ArgumentFailure

open PromptAssembly.ToolArgumentFailure

/-- The parser's detail names the field it found missing or did not know; a
type mismatch or a syntax error names a value or a position instead. This is
serde's error text, quoted in the notice after `Detail:`. -/
def namesField : FailureKind → Bool
  | .missingField | .unknownField => true
  | _ => false

private def started : ToolCallContext :=
  { callId := 0, requestId := 0, state := .pending, operation := .nativeCommand,
    deadline := 1, currentTime := 0, persistence := .uncommitted }

/-- The durable row of a call that started and was then refused for its
arguments. -/
def recorded : Option ToolCallContext :=
  (started.step? .dispatch).bind (·.step? (.fail .argumentInvalid))

def recordedState : String := (recorded.map (·.state.toDefraDB)).getD ""

def recordedClass : Option String := recorded.bind (·.failureClass.map FailureClass.toDefraDB)

/-- Whether this one refused call ends the request. -/
def endsRequest : Bool :=
  CompletionRetry.InvalidToolProgress.exhausted
    (CompletionRetry.InvalidToolProgress.recordDurable {} .invalidArguments)

theorem missing_tool_args_notify_model_and_terminalize_failed :
    recordedState = "failed" ∧ recordedClass = some "argumentInvalid" ∧ endsRequest = false ∧
      classify .unreported .missingField = .missingField ∧ namesField .missingField = true := by
  native_decide

end ToolExecution.ArgumentFailure
