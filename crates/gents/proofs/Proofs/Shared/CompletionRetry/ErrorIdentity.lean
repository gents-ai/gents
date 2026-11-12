import Mathlib.Data.Nat.Pairing
import Proofs.Shared.CompletionRetry.RepeatedToolFailure

/-!
# The identity of an ordinary tool failure

The repeated-failure guard counts a failure by its identity, which the owner of
the result derives. A tool that renders the native command envelope declares
which metadata fields are volatile (its duration and hint) and the identity
omits them, so a command that fails the same way each time is recognised as
repeating. Any other tool's error text is its identity whole, even when the
text happens to look like an envelope: a hint that changes between calls is a
different error.

Rust owner: `gents_loop::loop_stream::repeated_tool_failure::error_identity`,
with the envelope declared by `ToolDyn::emits_command_envelope`.
-/

namespace CompletionRetry.ErrorIdentity

open CompletionRetry.RepeatedToolFailure

/-- An envelope-shaped error: the fields that identify the failure and the
volatile metadata beside them. -/
structure ErrorText where
  stable : Nat
  volatile : Nat
  deriving DecidableEq, Repr

def errorIdentity (commandEnvelope : Bool) (text : ErrorText) : Nat :=
  if commandEnvelope then Nat.pair text.stable 0 else Nat.pair text.stable (text.volatile + 1)

/-- The same call failing `calls` times with the same stable error; the hint
changes on every call when `varyingHint`. -/
def failures (commandEnvelope varyingHint : Bool) (calls : Nat) : List Event :=
  (List.range calls).map fun call =>
    { call := 0, outcome := .ordinaryFailure,
      error := some (errorIdentity commandEnvelope ⟨0, if varyingHint then call else 0⟩) }

theorem envelope_identity_ignores_volatile_fields (stable left right : Nat) :
    errorIdentity true ⟨stable, left⟩ = errorIdentity true ⟨stable, right⟩ := by
  simp [errorIdentity]

theorem other_tools_keep_volatile_fields (stable left right : Nat) (h : left ≠ right) :
    errorIdentity false ⟨stable, left⟩ ≠ errorIdentity false ⟨stable, right⟩ := by
  intro heq
  simp [errorIdentity, Nat.pair_eq_pair] at heq
  omega

theorem envelope_shaped_errors_from_other_tools_keep_their_hint :
    run {} (failures false true 5) = (List.replicate 5 .dispatch, none) := by
  native_decide

theorem a_command_failing_with_a_changing_hint_still_stops :
    (run {} (failures true true 5)).2 = some .repeatedFailure := by
  native_decide

end CompletionRetry.ErrorIdentity
