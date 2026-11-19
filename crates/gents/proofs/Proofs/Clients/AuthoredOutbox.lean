namespace ClientAuthoredOutbox

inductive Reservation where
  | insert | retained | refused
  deriving BEq, DecidableEq, Repr

/-- Local retention precedes remote preparation. An existing submission keeps
its exact authored binding, including after runtime acknowledgement. -/
def reserve (localSigner bindingValid retainedExists same : Bool) : Reservation :=
  if !localSigner || !bindingValid then .refused
  else if !retainedExists then .insert
  else if same then .retained else .refused

/-- A remote preparation is written once. Losing an acknowledgement cannot
replace its signature, identity, expiry or request input. -/
def prepare (bindingMatches pending retainedExists same : Bool) : Reservation :=
  if !bindingMatches then .refused
  else if !pending then .retained
  else if !retainedExists then .insert
  else if same then .retained else .refused

/-- Only the bound runtime's confirmed receipt removes an authored intent
from the pending queue. Unreachability and denied access retain it. -/
def acknowledge (bindingMatches runtimeConfirmed : Bool) : Bool :=
  bindingMatches && runtimeConfirmed

/-- Ordinary reconnect delivery selects the earliest retained intent. Explicit
steering selects only the named pending intent and leaves its neighbours. -/
def select (pending : List String) (steer : Option String) : Option String :=
  match steer with
  | none => pending.head?
  | some id => if pending.contains id then some id else none

def reservationToken : Reservation → String
  | .insert => "insert" | .retained => "retained" | .refused => "refused"

theorem changed_input_is_refused (localSigner bindingValid : Bool) :
    reserve localSigner bindingValid true false = .refused := by
  cases localSigner <;> cases bindingValid <;> rfl

theorem unavailable_runtime_retains_intent (bindingMatches : Bool) :
    acknowledge bindingMatches false = false := by simp [acknowledge]

theorem unrelated_receipt_retains_intent (confirmed : Bool) :
    acknowledge false confirmed = false := by simp [acknowledge]

theorem confirmed_preparation_is_not_replaced (retainedExists same : Bool) :
    prepare true false retainedExists same = .retained := rfl

theorem ordinary_delivery_is_fifo (first : String) (rest : List String) :
    select (first :: rest) none = some first := rfl

theorem explicit_steer_keeps_queue (pending : List String) (id : String)
    (present : pending.contains id = true) : select pending (some id) = some id := by
  change (if pending.contains id = true then some id else none) = some id
  rw [if_pos present]

/-- A new chat derives its missing session identity from the stable authored
submission identity. Replaying the same input cannot allocate another session. -/
def sessionBinding (submission : String) (session : Option String) : String :=
  session.getD submission

theorem session_binding_replay (submission : String) :
    sessionBinding submission none = submission := rfl

end ClientAuthoredOutbox
