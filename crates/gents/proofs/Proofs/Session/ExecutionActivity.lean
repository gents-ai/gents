namespace Session.ExecutionActivity

inductive Phase where
  | queued | generating | toolWait | providerRetry | unavailable | terminal
  deriving DecidableEq, Repr

structure Identity where
  request : String
  generation : String
  deriving DecidableEq, Repr

structure Observation where
  identity : Identity
  sequence : Nat
  turn : Nat
  attempt : Nat
  observedAt : Nat
  phase : Phase
  deriving DecidableEq, Repr

/-- Observations refine the active generation only. Sequence is assigned by
the owner and is monotonic across provider attempts within that generation. -/
def accept (active : Identity) (current : Option Observation) (incoming : Observation) : Option Observation :=
  if incoming.identity != active then current
  else match current with
    | none => some incoming
    | some previous =>
        if previous.identity != active || (incoming.sequence > previous.sequence &&
            (incoming.turn > previous.turn || (incoming.turn = previous.turn && incoming.attempt ≥ previous.attempt))) then some incoming
        else current

inductive Display where
  | unknown | queued | working | waiting | retrying | unavailable | terminal
  deriving DecidableEq, Repr

/-- Terminal lifecycle is authoritative. An ephemeral observation that is
missing, stale, from the future, or from another generation is not activity. -/
def project (active : Identity) (terminal : Bool) (now maxAge : Nat)
    (observation : Option Observation) : Display :=
  if terminal then .terminal
  else match observation with
    | none => .unknown
    | some o =>
        if o.identity != active || o.observedAt > now || now - o.observedAt > maxAge then .unknown
        else match o.phase with
          | .queued => .queued
          | .generating => .working
          | .toolWait => .waiting
          | .providerRetry => .retrying
          | .unavailable => .unavailable
          | .terminal => .unknown

theorem terminal_wins (active : Identity) (now age : Nat) (observation : Option Observation) :
    project active true now age observation = .terminal := by simp [project]

theorem foreign_observation_is_ignored (active : Identity) (current : Option Observation)
    (incoming : Observation) (foreign : incoming.identity ≠ active) :
    accept active current incoming = current := by simp [accept, foreign]

theorem missing_observation_is_unknown (active : Identity) (now age : Nat) :
    project active false now age none = .unknown := by simp [project]

theorem late_sequence_is_ignored (active : Identity) (previous incoming : Observation)
    (same : incoming.identity = active) (previousSame : previous.identity = active)
    (late : incoming.sequence ≤ previous.sequence) :
    accept active (some previous) incoming = some previous := by
  simp [accept, same, previousSame, Nat.not_lt_of_ge late]

end Session.ExecutionActivity
