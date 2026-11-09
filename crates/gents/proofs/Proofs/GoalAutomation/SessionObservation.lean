import Proofs.GoalAutomation.RequestAssignment

/-!
The client's read of one session's signed Goal projection, what that read
shows the User, and how a send binds its Goal assignment from it.

`Observation.lean` decides which projection envelope is acceptable;
`RequestAssignment.prepareInput` decides the assignment a send captures. This
owner sits between them: it classifies one read transaction of the session
row and its projection into exactly one state, and projects that state to the
view the client shows. Rust owner: `gents::goal::observe_session_goal`
(consumed by `load_session_goal_observation_state_in_txn`), its desktop
projection `SessionGoalState`, and `bind_goal_input`.
-/

namespace GoalAutomation.SessionObservation

open GoalAutomation.RequestAssignment

/-- What the one session-row query of a read transaction returned.

External premise (DefraDB document ACP): a query filters rows the actor cannot
read without raising an error, so `absent` means only that no row is readable
by this actor. It is not a proof that the session does not exist. `omitted`
is a successful response that carries no row list for the collection at all;
it proves neither presence nor absence. -/
inductive SessionRead where
  | absent
  | present
  | omitted
  deriving DecidableEq, Repr

/-- The session's projection row, read in the same transaction as its session.
`verified` already passed `Observation.accepts` for the current physical
session, requester and selected Agent. -/
inductive ProjectionRead where
  | missing
  | unverified
  | verified (goal : Option Reference)
  deriving DecidableEq, Repr

/-- Why a readable session's Goal state is not known. -/
inductive UnknownCause where
  /-- The session response carried no row list. -/
  | readIncomplete
  /-- The Agent has not published a projection for this physical session. -/
  | projectionMissing
  /-- A projection exists but its signature or scope does not verify. -/
  | projectionUnverified
  deriving DecidableEq, Repr

inductive State where
  | noReadableSession
  | unknown (cause : UnknownCause)
  | observed (goal : Option Reference)
  deriving DecidableEq, Repr

/-- One read transaction classifies into exactly one state. Only a verified
envelope establishes a Goal or its absence. A response that omits its row list
is `unknown`, never an error the caller cannot act on and never
`noReadableSession`: it is not the empty list ACP filtering produces. -/
def observe : SessionRead → ProjectionRead → State
  | .omitted, _ => .unknown .readIncomplete
  | .absent, _ => .noReadableSession
  | .present, .missing => .unknown .projectionMissing
  | .present, .unverified => .unknown .projectionUnverified
  | .present, .verified goal => .observed goal

/-- The next step the view offers. -/
inductive NextAction where
  /-- Change the shown Goal (edit, pause, resume, stop). -/
  | controlGoal
  /-- Start a Goal: verified absence. -/
  | startGoal
  /-- Send a message: no readable session yet, so a send captures no
  inherited assignment and the runtime opens the session. -/
  | send
  /-- Read the observation again. The read owner holds no retry loop; who
  issues the refresh (the User or a client trigger) is product behavior. -/
  | refresh
  deriving DecidableEq, Repr

inductive Cause where
  | sessionNotReadable
  | unknown (cause : UnknownCause)
  deriving DecidableEq, Repr

/-- What the client shows. `goal` and `noGoal` are verified facts; every other
view names its cause. -/
inductive View where
  | goal (goal : Reference)
  | noGoal
  | unavailable (cause : Cause)
  deriving DecidableEq, Repr

def view : State → View
  | .observed (some goal) => .goal goal
  | .observed none => .noGoal
  | .noReadableSession => .unavailable .sessionNotReadable
  | .unknown cause => .unavailable (.unknown cause)

def next : State → NextAction
  | .observed (some _) => .controlGoal
  | .observed none => .startGoal
  | .noReadableSession => .send
  | .unknown _ => .refresh

/-- The send facts this state supplies to the existing assignment owner. -/
def inputFacts (captured : Option Reference) : State → InputFacts
  | .noReadableSession => ⟨captured, false, false, none⟩
  | .unknown _ => ⟨captured, true, false, none⟩
  | .observed goal => ⟨captured, true, true, goal⟩

def bind (captured : Option Reference) (state : State) : PreparedInput :=
  prepareInput (inputFacts captured state)

theorem unknown_is_not_absence (cause : UnknownCause) :
    view (.unknown cause) ≠ .noGoal := by
  simp [view]

theorem no_readable_session_is_not_absence : view .noReadableSession ≠ .noGoal := by
  simp [view]

/-- Only a verified envelope without a Goal is shown as "no Goal". -/
theorem no_goal_only_from_verified_absence (session : SessionRead)
    (projection : ProjectionRead) (shown : view (observe session projection) = .noGoal) :
    session = .present ∧ projection = .verified none := by
  cases session <;> cases projection <;> simp_all [observe, view]
  case present.verified goal => cases goal <;> simp_all

theorem omitted_rows_are_unknown (projection : ProjectionRead) :
    observe .omitted projection = .unknown .readIncomplete := by
  cases projection <;> rfl

theorem omitted_rows_are_not_no_readable_session (projection : ProjectionRead) :
    observe .omitted projection ≠ .noReadableSession := by
  cases projection <;> simp [observe]

theorem bind_refuses_on_unknown (cause : UnknownCause) :
    bind none (.unknown cause) = .pending := by
  simp [bind, inputFacts, prepareInput]

theorem omitted_rows_refuse_send (projection : ProjectionRead) :
    bind none (observe .omitted projection) = .pending := by
  rw [omitted_rows_are_unknown]; exact bind_refuses_on_unknown _

/-- A refused send names its cause and offers a refresh: the User is never
left with a refusal that has no cause or next step. -/
theorem pending_names_cause_and_refresh (captured : Option Reference) (state : State)
    (refused : bind captured state = .pending) :
    (∃ cause, view state = .unavailable (.unknown cause)) ∧ next state = .refresh := by
  cases captured <;> cases state <;> simp_all [bind, inputFacts, prepareInput, view, next]

/-- Every view is either a verified fact or names its cause, and every state
offers a next action that can change it; none is silently stuck. -/
theorem every_view_is_a_fact_or_names_its_cause (state : State) :
    (∃ goal, view state = .goal goal ∧ next state = .controlGoal) ∨
    (view state = .noGoal ∧ next state = .startGoal) ∨
    (view state = .unavailable .sessionNotReadable ∧ next state = .send) ∨
    (∃ cause, view state = .unavailable (.unknown cause) ∧ next state = .refresh) := by
  cases state with
  | noReadableSession => simp [view, next]
  | unknown cause => simp [view, next]
  | observed goal => cases goal <;> simp [view, next]

/-- A send from a session with no readable row binds no inherited assignment. -/
theorem no_readable_session_binds_nothing :
    bind none .noReadableSession = .prepared none := by
  simp [bind, inputFacts, prepareInput]

end GoalAutomation.SessionObservation
