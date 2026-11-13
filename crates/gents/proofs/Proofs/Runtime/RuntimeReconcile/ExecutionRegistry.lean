import Proofs.Runtime.RuntimeReconcile.ServiceReplacement

/-!
# Background execution registry

The registry keys each owned background invocation by its tool-call document.
Every reservation holds one runtime admission (`ServiceReplacement.step .admit`)
until its owner releases it, so retirement stays busy while any reservation
lives, and a stopping runtime refuses a new reservation before it is recorded.
-/

namespace Background.ExecutionRegistry

open RuntimeReconcile.ServiceReplacement

inductive Refusal
  | unauthorized
  | buildChanged
  | busy
  | stopping
  | unavailable
  | duplicate
  deriving DecidableEq, Repr

structure Registry where
  admission : State
  live : List String
  deriving DecidableEq, Repr

def empty : Registry :=
  { admission := { phase := .accepting, executions := 0, nativeOutstanding := 0 }, live := [] }

/-- A reservation is admitted before the document is checked; a document that
already has an owner is refused and its admission is released at once. -/
def reserve (r : Registry) (doc : String) : Registry × Option Refusal :=
  match step r.admission .admit with
  | none => (r, some (if r.admission.phase = .accepting then .unavailable else .stopping))
  | some next =>
    if doc ∈ r.live then (r, some .duplicate)
    else ({ admission := next, live := r.live ++ [doc] }, none)

/-- Releasing a held reservation removes its entry and finishes its admission. -/
def release (r : Registry) (doc : String) : Registry :=
  if doc ∈ r.live then
    match step r.admission .finish with
    | some next => { admission := next, live := r.live.erase doc }
    | none => { r with live := r.live.erase doc }
  else r

def retire (r : Registry) (authority : RetirementAuthority) : Registry × Option Refusal :=
  if !authority.operatorAuthenticated then (r, some .unauthorized)
  else if !authority.observedBuildMatches then (r, some .buildChanged)
  else
    match step r.admission (.retire authority) with
    | some next => ({ r with admission := next }, none)
    | none => (r, some (if r.admission.phase = .accepting then .busy else .stopping))

inductive Action
  | reserve (doc : String)
  | release (doc : String)
  | retire (authority : RetirementAuthority)
  deriving DecidableEq, Repr

def apply (r : Registry) : Action → Registry × Option Refusal
  | .reserve doc => reserve r doc
  | .release doc => (release r doc, none)
  | .retire authority => retire r authority

def run : Registry → List Action → List (Option Refusal × List String)
  | _, [] => []
  | r, a :: rest =>
    let (next, refusal) := apply r a
    (refusal, next.live) :: run next rest

private def operator : RetirementAuthority := ⟨true, true⟩

theorem background_ownership_blocks_retirement_and_stopping_refuses_new_reservations :
    run empty [.reserve "owned-doc", .retire operator, .release "owned-doc", .retire operator,
      .reserve "refused-doc"] =
      [(none, ["owned-doc"]), (some .busy, ["owned-doc"]), (none, []), (none, []),
       (some .stopping, [])] := by native_decide

theorem replacing_a_registry_entry_does_not_release_the_original_workers_retirement_guard :
    run empty [.reserve "old-doc", .reserve "new-doc", .release "new-doc", .retire operator,
      .release "old-doc", .retire operator] =
      [(none, ["old-doc"]), (none, ["old-doc", "new-doc"]), (none, ["old-doc"]),
       (some .busy, ["old-doc"]), (none, []), (none, [])] := by native_decide

theorem a_second_owner_of_one_document_is_refused_and_holds_nothing :
    run empty [.reserve "doc", .reserve "doc", .release "doc", .retire operator] =
      [(none, ["doc"]), (some .duplicate, ["doc"]), (none, []), (none, [])] := by native_decide

theorem a_live_reservation_keeps_retirement_busy (r : Registry) (doc : String)
    (accepting : r.admission.phase = .accepting) (fresh : doc ∉ r.live) :
    (retire (reserve r doc).1 operator).2 = some .busy := by
  simp [reserve, step, accepting, fresh, retire, operator]

end Background.ExecutionRegistry
