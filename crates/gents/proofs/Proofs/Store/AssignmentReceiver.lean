/-! A receiver acts only on complete certified assignments. -/

namespace AssignmentReceiver

structure Position where
  term : Nat
  height : Nat
  hash : String
  deriving DecidableEq, Repr

def accepts (previous current : Position) : Bool :=
  current.term > previous.term ||
    (current.term == previous.term &&
      (current.height > previous.height ||
        (current.height == previous.height && current.hash == previous.hash)))

theorem an_older_term_is_rejected (previous current : Position)
    (h : current.term < previous.term) : accepts previous current = false := by
  simp [accepts, Nat.not_lt_of_ge (Nat.le_of_lt h), Nat.ne_of_lt h]

inductive Read where
  | unavailable
  | invalid
  | complete (position : Position) (assigned : List String)
  deriving DecidableEq, Repr

structure State where
  position : Option Position
  assigned : List String
  deriving DecidableEq, Repr

def observe (state : State) (read : Read) : State :=
  match read with
  | .unavailable | .invalid => state
  | .complete position assigned =>
      if state.position == some position && state.assigned != assigned then state
      else if state.position.all (fun previous => accepts previous position) then
        ⟨some position, assigned⟩
      else state

def permits (assigned bodyVerified : Bool) : Bool := assigned && bodyVerified

def retry (unavailable overtaken : Bool) : Bool := unavailable || overtaken

/-- A closed shutdown channel and a true value both stop runtime callbacks. -/
def stopForShutdown (requested closed : Bool) : Bool := requested || closed

theorem false_update_keeps_callback : stopForShutdown false false = false := rfl

theorem closed_channel_stops_callback (requested : Bool) :
    stopForShutdown requested true = true := by simp [stopForShutdown]

def mayRun (state : State) (id : String) (bodyVerified : Bool) : Bool :=
  permits (state.assigned.contains id) bodyVerified

theorem an_unavailable_read_changes_nothing (state : State) :
    observe state .unavailable = state := rfl

theorem a_missing_body_cannot_run (state : State) (id : String) :
    mayRun state id false = false := by simp [mayRun, permits]

theorem a_withdrawn_assignment_cannot_run (state : State) (id : String)
    (h : state.assigned.contains id = false) : mayRun state id true = false := by
  unfold mayRun permits
  rw [h]
  rfl

end AssignmentReceiver
