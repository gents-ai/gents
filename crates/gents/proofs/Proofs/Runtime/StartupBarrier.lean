import Lean

namespace Runtime.StartupBarrier
open Lean

structure Slot where
  agent : String
  generation : Nat
  deriving BEq, ToJson

inductive Operation where
  | register | ready | demote | supersede
  deriving BEq, ToJson

structure Action where
  operation : Operation
  slot : Slot
  deriving ToJson

structure Input where
  seed : List String
  actions : List Action
  deriving ToJson

structure Observation where
  pending : List Slot
  ready : Bool
  released : Option Bool
  deriving ToJson

private def ordered (slots : List Slot) : List Slot :=
  slots.mergeSort fun a b => if a.agent == b.agent then a.generation ≤ b.generation else a.agent ≤ b.agent

/-- Readiness obligations are keyed by both Agent and generation. Every release
kind retires exactly its key; readiness means no obligation remains. -/
def step (pending : List Slot) (action : Action) : List Slot × Option Bool :=
  if action.operation == .register then
    (ordered (if pending.contains action.slot then pending else action.slot :: pending), none)
  else (pending.filter (· != action.slot), some (pending.contains action.slot))

def observe (pending : List Slot) (released : Option Bool) : Observation :=
  ⟨pending, pending.isEmpty, released⟩

def run (input : Input) : List Observation := Id.run do
  let mut pending := ordered (input.seed.map fun agent => ⟨agent, 1⟩)
  let mut result := [observe pending none]
  for action in input.actions do
    let (next, released) := step pending action
    pending := next
    result := result ++ [observe pending released]
  return result

end Runtime.StartupBarrier
