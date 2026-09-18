import Mathlib.Data.Finset.Basic

namespace ToolPolicy.WriteInput

inductive Kind where
  | text | integer | number | boolean | array | object | null
  deriving DecidableEq, Repr

def compatible (expected actual : Kind) : Bool :=
  expected == actual || (expected == .number && actual == .integer)

/-- Caller input never supplies runtime-filled metadata. Missing optional fields
remain absent; explicit null is distinct and requires a nullable schema field. -/
def admits (expected : Kind) (nullable required filled : Bool)
    (actual : Option Kind) : Bool :=
  if filled then actual.isNone else
    match actual with
    | none => !required
    | some .null => nullable
    | some kind => compatible expected kind

theorem filled_rejects_caller_value (expected actual : Kind) (nullable required : Bool) :
    admits expected nullable required true (some actual) = false := by
  simp [admits]

theorem required_rejects_absence (expected : Kind) (nullable : Bool) :
    admits expected nullable true false none = false := by
  simp [admits]

theorem nonnullable_rejects_null (expected : Kind) (required : Bool) :
    admits expected false required false (some .null) = false := by
  simp [admits]

theorem integer_rejects_string (nullable required : Bool) :
    admits .integer nullable required false (some .text) = false := by
  decide +revert

theorem boolean_rejects_string (nullable required : Bool) :
    admits .boolean nullable required false (some .text) = false := by
  decide +revert

theorem nonnull_admission_preserves_kind (expected actual : Kind)
    (nullable required : Bool) (notNull : actual ≠ .null)
    (accepted : admits expected nullable required false (some actual) = true) :
    compatible expected actual = true := by
  cases actual <;> simp_all [admits]

/-- A supplied workflow correlation wins. Otherwise invocation-local correlation
is the existing request identity, never a new notification identity. -/
def invocationCorrelation (request supplied : Option String) : Option String :=
  match supplied.filter (fun value => value.trim != "") with
  | some value => some value
  | none => request.filter (fun value => value.trim != "")

theorem explicit_correlation_preserved (request : Option String) (value : String)
    (valid : value.trim ≠ "") :
    invocationCorrelation request (some value) = some value := by
  simp [invocationCorrelation, Option.filter, valid]

theorem uncorrelated_invocation_uses_request (request : String)
    (valid : request.trim ≠ "") :
    invocationCorrelation (some request) none = some request := by
  simp [invocationCorrelation, Option.filter, valid]

theorem absent_identity_fails_closed : invocationCorrelation none none = none := by
  rfl

end ToolPolicy.WriteInput
