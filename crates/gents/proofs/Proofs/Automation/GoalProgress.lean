import Proofs.Automation.Goals

namespace GoalProgress

/-- One tool call of a goal turn as the harness observed it: the tool's name,
its arguments in canonical JSON (keys sorted, no insignificant whitespace) and
a digest of its result. The Agent's text is not part of it, so rewording what
the Agent says never changes whether a turn made progress. -/
structure CallPrint where
  tool : String
  arguments : String
  result : String
  deriving DecidableEq, Repr

/-- What the harness observed of one finished goal turn: the prints of its
tool calls, and whether the turn changed the Goal's state (an `update_goal`
the Goal owner applied). -/
structure Turn where
  calls : List CallPrint
  goalStateChanged : Bool
  deriving DecidableEq, Repr

/-- Consecutive goal turns without progress after which the harness pauses a
Goal. Premise: three empty automatic turns is the shipped threshold of the
nearest comparable runtime, and repeated identical actions predict failure
rather than eventual success, so a fourth identical turn mostly buys cost. -/
def noProgressLimit : Nat := 3

/-- A turn makes progress iff it changed the Goal's state or made a tool call
whose print was not among the previous goal turn's prints. A turn with no
tool calls, or only calls identical in name, arguments and result to the
previous turn's, makes none. -/
def madeProgress (previous : List CallPrint) (turn : Turn) : Bool :=
  turn.goalStateChanged || turn.calls.any (fun call => !previous.contains call)

/-- How the harness counts a finished goal turn. A Goal waiting on its own
live background work (a running tool call whose deadline has not passed) is
in a verified wait; once that deadline passes, the turn counts as any other. -/
inductive Observation where
  | progress
  | verifiedWait
  | noProgress
  deriving DecidableEq, Repr

def observe (previous : List CallPrint) (turn : Turn) (liveDeadline : Option Nat)
    (now : Nat) : Observation :=
  if madeProgress previous turn then .progress
  else if liveDeadline.any (now < ·) then .verifiedWait
  else .noProgress

/-- The no-progress record the Goal owner keeps on the Goal when it decides a
continuation: the streak of turns without progress and the prints of the
latest turn. -/
structure Record where
  streak : Nat
  previous : List CallPrint
  deriving DecidableEq, Repr

def Record.fresh : Record := ⟨0, []⟩

/-- The owner's step over one finished goal turn: the record it writes and
whether the Goal pauses. A verified wait neither extends nor clears the
streak. -/
def step (record : Record) (turn : Turn) (liveDeadline : Option Nat) (now : Nat) :
    Record × Bool :=
  match observe record.previous turn liveDeadline now with
  | .progress => (⟨0, turn.calls⟩, false)
  | .verifiedWait => (⟨record.streak, turn.calls⟩, false)
  | .noProgress =>
      let streak := record.streak + 1
      (⟨streak, turn.calls⟩, noProgressLimit ≤ streak)

theorem a_repeated_turn_makes_no_progress (calls : List CallPrint) :
    madeProgress calls ⟨calls, false⟩ = false := by
  simp only [madeProgress, Bool.false_or, List.any_eq_false, Bool.not_eq_eq_eq_not,
    Bool.not_true]
  intro call member
  simpa using member

theorem a_turn_without_calls_makes_no_progress (previous : List CallPrint) :
    madeProgress previous ⟨[], false⟩ = false := rfl

theorem a_new_call_is_progress (previous : List CallPrint) (call : CallPrint)
    (rest : List CallPrint) (changed : Bool) (fresh : previous.contains call = false) :
    madeProgress previous ⟨call :: rest, changed⟩ = true := by
  have absent : call ∉ previous := by simpa using fresh
  simp [madeProgress, absent]

theorem a_goal_state_change_is_progress (previous calls : List CallPrint) :
    madeProgress previous ⟨calls, true⟩ = true := rfl

/-- A wait is verified only before its job's deadline: from the deadline on,
the turn is observed exactly as if no job were live. -/
theorem an_expired_wait_is_not_a_wait (previous : List CallPrint) (turn : Turn)
    (deadline now : Nat) (expired : deadline ≤ now) :
    observe previous turn (some deadline) now = observe previous turn none now := by
  have : ¬ now < deadline := Nat.not_lt.mpr expired
  simp [observe, this]

/-- Turns without progress after every live deadline has passed pause the
Goal on the `noProgressLimit`-th of them, from a fresh record. -/
def run (record : Record) : List (Turn × Option Nat × Nat) → Record × Bool
  | [] => (record, false)
  | (turn, deadline, now) :: rest =>
      let (next, pause) := step record turn deadline now
      if pause then (next, true) else run next rest

theorem the_same_turn_repeated_pauses (call : CallPrint) (now : Nat) :
    (run Record.fresh
      [(⟨[call], false⟩, none, now), (⟨[call], false⟩, none, now),
       (⟨[call], false⟩, none, now), (⟨[call], false⟩, none, now)]).2 = true := by
  simp [run, step, observe, madeProgress, Record.fresh, noProgressLimit]

/-- An Agent cannot idle forever behind a wait: once now is past its job's
deadline, `noProgressLimit` turns that repeat the previous turn pause the
Goal, whatever streak the record holds. -/
theorem waiting_past_the_deadline_pauses (record : Record) (deadline now : Nat)
    (expired : deadline ≤ now) :
    let turn : Turn := ⟨record.previous, false⟩
    (run record (List.replicate noProgressLimit (turn, some deadline, now))).2 = true := by
  have hn : ¬ now < deadline := Nat.not_lt.mpr expired
  have hp : ∀ calls : List CallPrint, madeProgress calls ⟨calls, false⟩ = false :=
    a_repeated_turn_makes_no_progress
  rcases record with ⟨streak, previous⟩
  simp only [noProgressLimit, List.replicate, run, step, observe, hp, hn, Option.any_some,
    decide_false, Bool.false_eq_true, if_false]
  by_cases h1 : 3 ≤ streak + 1
  · simp [h1]
  · by_cases h2 : 3 ≤ streak + 1 + 1
    · simp [h1, h2]
    · have h3 : 3 ≤ streak + 1 + 1 + 1 := by omega
      simp [h1, h2, h3]

end GoalProgress
