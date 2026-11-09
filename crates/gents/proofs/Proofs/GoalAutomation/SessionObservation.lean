import Proofs.GoalAutomation.RequestAssignment

/-!
The client's read of one session's signed Goal projection, what that read
shows the User, and how a send binds its Goal assignment from it.

`Observation.lean` decides which projection envelope is acceptable;
`RequestAssignment.prepareInput` decides the assignment a send captures. This
owner sits between them: it classifies one read transaction of the session
row and its projection into exactly one state, and projects that state to the
view and next action the client shows. Rust owner: `gents::goal`
(`observe_session_goal`, `SessionGoalObservationState::next`,
`bind_goal_input` and its `GoalInputRefused`), read by
`load_client_session_goal_observation_state`; the desktop DTO
`SessionGoalState` only carries these.
-/

namespace GoalAutomation.SessionObservation

open GoalAutomation.RequestAssignment

/-- What the one session-row query of a read transaction returned. The read is
scoped by node and session id only; the projection is then read for the row's
own requester provenance, so a client never chooses another requester's scope.

External premise (DefraDB document ACP): a query filters rows the actor cannot
read without raising an error, so `absent` means only that no row is readable
by this actor. It is not a proof that the session does not exist.

A successful response that omits a collection's row list is not a state: real
DefraDB returns an empty list for a filtered collection, so omission is a
malformed response, and both reads of the transaction (session row and
projection) fail it as a read error rather than classify it. -/
inductive SessionRead where
  | absent
  | present
  deriving DecidableEq, Repr

/-- The session's projection row, read in the same transaction as its session.
`verified` already passed `Observation.accepts` for the current physical
session, the session row's requester and its selected Agent. -/
inductive ProjectionRead where
  | missing
  | unverified
  | verified (goal : Option Reference)
  deriving DecidableEq, Repr

/-- Why a readable session's Goal state is not known. -/
inductive UnknownCause where
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
envelope establishes a Goal or its absence. -/
def observe : SessionRead → ProjectionRead → State
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
  /-- Wait for the Agent's runtime to publish a verifiable projection, then act
  again; every send and Goal change re-reads the observation.

  External premise (`goal::reconcile_goal_observations_with_identities`): the
  runtime's sweep, on change notifications and on its periodic tick, publishes
  a missing projection for each session of a running Agent and rewrites one
  that does not verify (`project_session_goal`). Its one exception is a row
  whose immutable requester scope differs from its session: the sweep reports
  that session as unresolved instead of rewriting it. A client holds no retry
  loop; the next action is the User's. -/
  | awaitPublication
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
  | .unknown _ => .awaitPublication

/-- The send facts this state supplies to the existing assignment owner. -/
def inputFacts (captured : Option Reference) : State → InputFacts
  | .noReadableSession => ⟨captured, false, false, none⟩
  | .unknown _ => ⟨captured, true, false, none⟩
  | .observed goal => ⟨captured, true, true, goal⟩

def bind (captured : Option Reference) (state : State) : PreparedInput :=
  prepareInput (inputFacts captured state)

/-- The cause a refused send carries (Rust `GoalInputRefused`). -/
def refusal (captured : Option Reference) (state : State) : Option UnknownCause :=
  match bind captured state, state with
  | .pending, .unknown cause => some cause
  | _, _ => none

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

theorem bind_refuses_on_unknown (cause : UnknownCause) :
    bind none (.unknown cause) = .pending := by
  simp [bind, inputFacts, prepareInput]

/-- A refused send carries exactly the cause its view names, and offers the
same next action: the User is never left with a refusal that has no cause or
next step, and a send and the view never disagree about why. -/
theorem refused_send_names_the_view_cause (captured : Option Reference) (state : State)
    (refused : bind captured state = .pending) :
    ∃ cause, refusal captured state = some cause ∧
      view state = .unavailable (.unknown cause) ∧ next state = .awaitPublication := by
  cases captured <;> cases state <;>
    simp_all [refusal, bind, inputFacts, prepareInput, view, next]

/-- Only a refused send carries a cause. -/
theorem refusal_only_when_pending (captured : Option Reference) (state : State)
    (cause : UnknownCause) (named : refusal captured state = some cause) :
    bind captured state = .pending := by
  unfold refusal at named
  split at named <;> simp_all

/-- Every view is either a verified fact or names its cause, with one next
action per state. -/
theorem every_view_is_a_fact_or_names_its_cause (state : State) :
    (∃ goal, view state = .goal goal ∧ next state = .controlGoal) ∨
    (view state = .noGoal ∧ next state = .startGoal) ∨
    (view state = .unavailable .sessionNotReadable ∧ next state = .send) ∨
    (∃ cause, view state = .unavailable (.unknown cause) ∧
      next state = .awaitPublication) := by
  cases state with
  | noReadableSession => simp [view, next]
  | unknown cause => simp [view, next]
  | observed goal => cases goal <;> simp [view, next]

/-- `awaitPublication` makes progress: whatever unknown cause a readable
session shows, the Agent's publication of a verifiable projection makes the
next read observed, a verified fact the User can act on, and the send binds
without refusal. -/
theorem publication_resolves_unknown (projection : ProjectionRead) (cause : UnknownCause)
    (unknown : observe .present projection = .unknown cause) (goal : Option Reference)
    (captured : Option Reference) :
    observe .present (.verified goal) = .observed goal ∧
      next (.observed goal) ≠ .awaitPublication ∧
      bind captured (.observed goal) ≠ .pending := by
  cases projection <;> simp_all [observe]
  all_goals
    refine ⟨?_, ?_⟩
    · cases goal <;> simp [next]
    · cases captured <;> cases goal <;> simp [bind, inputFacts, prepareInput]

/-- A send from a session with no readable row binds no inherited assignment. -/
theorem no_readable_session_binds_nothing :
    bind none .noReadableSession = .prepared none := by
  simp [bind, inputFacts, prepareInput]

end GoalAutomation.SessionObservation
