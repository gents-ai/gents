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

end PromptAssembly.ToolArgumentFailure
