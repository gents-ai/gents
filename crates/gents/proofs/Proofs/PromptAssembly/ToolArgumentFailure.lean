import Proofs.Basic

namespace PromptAssembly.ToolArgumentFailure

inductive Termination
  | unreported | completed | tokenLimit
  deriving DecidableEq, Repr

inductive ParseFailure
  | syntax | unknownField | missingField | wrongType | schema
  deriving DecidableEq, Repr

inductive FailureKind
  | malformed | truncated | unknownField | missingField | wrongType | schema
  deriving DecidableEq, Repr

/-- JSON parser EOF is also produced by malformed, normally terminated output.
Only a token-limit observation from this provider attempt authenticates truncation.
Schema failures describe complete JSON and retain their own correction. -/
def classify (termination : Termination) : ParseFailure → FailureKind
  | .syntax => if termination = .tokenLimit then .truncated else .malformed
  | .unknownField => .unknownField
  | .missingField => .missingField
  | .wrongType => .wrongType
  | .schema => .schema

theorem truncation_requires_token_limit (termination : Termination) (failure : ParseFailure) :
    classify termination failure = .truncated → termination = .tokenLimit := by
  cases failure <;> simp [classify]

theorem token_limit_does_not_change_schema (termination : Termination) (failure : ParseFailure)
    (h : failure ≠ .syntax) : classify termination failure = classify .unreported failure := by
  cases failure <;> simp_all [classify]

theorem absent_reason_does_not_authenticate_truncation :
    classify .unreported .syntax = .malformed := rfl

/-- A delimited Responses terminal authenticates a token-budget stop only when
`incomplete_details.reason` is `max_output_tokens`. Other incomplete reasons
remain provider errors. Transport EOF never supplies terminal evidence.
https://developers.openai.com/api/reference/resources/responses/streaming-events -/
def responsesTermination (delimited : Bool) (eventType : String)
    (reason : Option String) : Option Termination :=
  if delimited then
    if eventType = "response.completed" then some .completed
    else if eventType = "response.incomplete" ∧ reason = some "max_output_tokens" then
      some .tokenLimit
    else none
  else none

theorem responses_token_limit_requires_evidence (delimited : Bool) (eventType : String)
    (reason : Option String)
    (h : responsesTermination delimited eventType reason = some .tokenLimit) :
    delimited = true ∧ eventType = "response.incomplete" ∧ reason = some "max_output_tokens" := by
  cases delimited <;>
    by_cases completed : eventType = "response.completed" <;>
    by_cases incomplete : eventType = "response.incomplete" <;>
    by_cases limited : reason = some "max_output_tokens" <;>
    simp_all [responsesTermination]

theorem responses_eof_does_not_authenticate (eventType : String) (reason : Option String) :
    responsesTermination false eventType reason = none := rfl

theorem responses_completed_is_not_token_limit (reason : Option String) :
    responsesTermination true "response.completed" reason = some .completed := by
  simp [responsesTermination]

end PromptAssembly.ToolArgumentFailure
