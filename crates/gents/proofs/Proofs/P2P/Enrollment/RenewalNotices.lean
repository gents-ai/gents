/-!
Renewal notices for one network's enrollment projection. A generation whose
lease needs renewal is noticed once per lease state. A generation that stops
being active is forgotten, so a superseding generation in the same state is
noticed again. Observations are taken in order; each carries the lease state
read at that observation time.
-/

namespace Enrollment.RenewalNotices

inductive LeaseState where
  | current
  | renewalDue
  | expired
  | invalid
  deriving DecidableEq, Repr

structure Observation where
  active : Bool
  sequence : Nat
  state : LeaseState
  deriving DecidableEq, Repr

structure Trace where
  steps : List Observation
  deriving Repr

abbrev Noticed := List (Nat × LeaseState)

def needsRenewal : LeaseState → Bool
  | .current => false
  | _ => true

def forget (noticed : Noticed) (o : Observation) : Noticed :=
  if o.active then noticed.filter (fun p => p.1 == o.sequence) else []

def stepNotice (noticed : Noticed) (o : Observation) : Noticed × Option LeaseState :=
  let kept := forget noticed o
  if o.active && needsRenewal o.state && decide ((o.sequence, o.state) ∉ kept) then
    ((o.sequence, o.state) :: kept, some o.state)
  else (kept, none)

def notices : Noticed → List Observation → List (Option LeaseState)
  | _, [] => []
  | noticed, o :: os =>
    let r := stepNotice noticed o
    r.2 :: notices r.1 os

theorem current_is_never_noticed (noticed : Noticed) (o : Observation)
    (h : o.state = .current) : (stepNotice noticed o).2 = none := by
  simp [stepNotice, needsRenewal, h]

theorem inactive_is_never_noticed (noticed : Noticed) (o : Observation)
    (h : o.active = false) : (stepNotice noticed o).2 = none := by
  simp [stepNotice, forget, h]

/-- What one observation reports: the notice it raised, naming the generation
and the lease state, and how many generation states remain remembered. -/
structure Notice where
  sequence : Nat
  state : LeaseState
  deriving DecidableEq, Repr

structure Observed where
  notice : Option Notice
  remembered : Nat
  deriving DecidableEq, Repr

def observeStep (noticed : Noticed) (o : Observation) : Noticed × Observed :=
  let r := stepNotice noticed o
  (r.1, { notice := r.2.map fun state => { sequence := o.sequence, state }
          remembered := r.1.length })

def observeAll : Noticed → List Observation → List Observed
  | _, [] => []
  | noticed, o :: os =>
    let r := observeStep noticed o
    r.2 :: observeAll r.1 os

theorem a_generation_is_noticed_once_per_state_and_forgotten_when_superseded :
    observeAll []
        [ ⟨true, 1, .current⟩, ⟨true, 1, .renewalDue⟩, ⟨true, 1, .renewalDue⟩
        , ⟨true, 1, .expired⟩, ⟨true, 1, .expired⟩, ⟨true, 2, .current⟩
        , ⟨true, 2, .renewalDue⟩ ] =
      [ ⟨none, 0⟩, ⟨some ⟨1, .renewalDue⟩, 1⟩, ⟨none, 1⟩, ⟨some ⟨1, .expired⟩, 2⟩
      , ⟨none, 2⟩, ⟨none, 0⟩, ⟨some ⟨2, .renewalDue⟩, 1⟩ ] := by
  decide

end Enrollment.RenewalNotices
