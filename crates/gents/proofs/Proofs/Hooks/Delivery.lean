import Mathlib.Data.List.Dedup

namespace Hooks.Delivery

/-- Delivery after commit. The terminal outcome and the keys of the
deliveries it owes are published by one transaction; each delivery then runs
in its own transaction through the same data contract (rows in, effects out).
A plugin whose staging was dropped at a must-commit point is owed the same
way (`Hooks.owed`). `owed` is the durable redrive record:
`runtime.recover.sweep` in gents-ai/gents#3642 is the loop over `pending`, not
a hook of its own. A delivery has no caller's transaction; it runs under the
actor recorded when it was owed, the actor of the owing transaction, never a
plugin or node identity (`Hooks.Safety.perform`).

Design choice: the record of what is owed is core's, keyed by the physical
subject and the plugin. The alternative, each feature keeping its own redrive
marker (`completionPending`, per-feature sweeps), is what exists today and is
why a delivery added at one terminal site is missed at another. -/
structure State (Outcome Key : Type) where
  terminal : Option Outcome
  owed : List Key
  delivered : List Key
  deriving DecidableEq, Repr

variable {Outcome Key : Type} [DecidableEq Key]

def publish (state : State Outcome Key) (outcome : Outcome) (keys : List Key)
    (commit : Bool) : State Outcome Key :=
  if commit && state.terminal.isNone then
    { state with terminal := some outcome, owed := (state.owed ++ keys).dedup }
  else state

/-- One delivery attempt. `applied` is whether the delivery's own transaction
committed; a refusal, an invalid effect or a crash is `false`. -/
def deliver (state : State Outcome Key) (key : Key) (applied : Bool) : State Outcome Key :=
  if applied && decide (key ∈ state.owed) && !decide (key ∈ state.delivered) then
    { state with delivered := key :: state.delivered }
  else state

def pending (state : State Outcome Key) : List Key :=
  state.owed.filter fun key => !decide (key ∈ state.delivered)

theorem rollback_publishes_nothing (state : State Outcome Key) (outcome : Outcome)
    (keys : List Key) : publish state outcome keys false = state := by
  simp [publish]

theorem terminal_is_published_once (state : State Outcome Key) (prior outcome : Outcome)
    (keys : List Key) (commit : Bool) (terminal : state.terminal = some prior) :
    publish state outcome keys commit = state := by
  simp [publish, terminal]

/-- A delivery, successful or not, cannot change the terminal outcome. -/
theorem delivery_cannot_change_terminal (state : State Outcome Key) (key : Key)
    (applied : Bool) : (deliver state key applied).terminal = state.terminal := by
  unfold deliver
  split <;> rfl

theorem delivery_cannot_change_what_is_owed (state : State Outcome Key) (key : Key)
    (applied : Bool) : (deliver state key applied).owed = state.owed := by
  unfold deliver
  split <;> rfl

/-- A failed delivery stays owed, so the sweep finds it again. -/
theorem failed_delivery_stays_pending (state : State Outcome Key) (key : Key) :
    pending (deliver state key false) = pending state := by
  simp [deliver]

theorem committed_delivery_is_settled (state : State Outcome Key) (key : Key)
    (owed : key ∈ state.owed) : key ∉ pending (deliver state key true) := by
  by_cases already : key ∈ state.delivered
  · simp [deliver, pending, owed, already]
  · simp [deliver, pending, owed, already]

/-- Delivery is at least once: a redelivered key is inert. -/
theorem redelivery_is_inert (state : State Outcome Key) (key : Key) (applied : Bool) :
    deliver (deliver state key true) key applied = deliver state key true := by
  by_cases owed : key ∈ state.owed <;> by_cases already : key ∈ state.delivered <;>
    cases applied <;> simp [deliver, owed, already]

/-- Nothing owed is lost: until its delivery commits, a key published with
the terminal is pending. -/
theorem published_delivery_is_pending (state : State Outcome Key) (outcome : Outcome)
    (keys : List Key) (key : Key) (fresh : state.terminal = none) (named : key ∈ keys)
    (undelivered : key ∉ state.delivered) :
    key ∈ pending (publish state outcome keys true) := by
  simp [publish, pending, fresh, named, undelivered, List.mem_dedup]

end Hooks.Delivery
