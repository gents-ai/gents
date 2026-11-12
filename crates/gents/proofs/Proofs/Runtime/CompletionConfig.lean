import Lean

namespace Runtime.CompletionConfig
open Lean

structure Input where
  origin : String
  transport : Option Int
  interactive : Option Int
  resample : Option Int
  repair : Option Bool
  delays : Option (List Int)
  hops : Nat
  deadline : Option Int
  deriving ToJson

structure Observation where
  delays : List Nat
  resample : Nat
  empty : Nat
  repair : Bool
  hops : Nat
  deadline : Option Int
  deriving ToJson

def nonnegative (value : Option Int) : Option Nat :=
  value.bind fun value => if value < 0 then none else some value.toNat

/-- Scheduled work has a longer transport ladder; an interactive override
only affects interactive work. Positive authored delays survive validation,
and resizing repeats the final delay instead of inventing a new cadence. -/
def resolve (input : Input) : Option Observation := do
  if input.origin != "interactive" && input.origin != "scheduled" then none else do
    let interactive := input.origin == "interactive"
    let defaultDelays := if interactive then [2000] else [5000, 30000, 120000]
    let delays := input.delays.map (fun values => values.filterMap fun value =>
      if value > 0 then some value.toNat else none)
    let delays := match delays with | some (first :: rest) => first :: rest | _ => defaultDelays
    let maximum := nonnegative (if interactive then input.interactive.orElse (fun _ => input.transport) else input.transport)
    let delays := match maximum with
      | none => delays
      | some maximum => (List.range maximum).map fun index => (delays[index]?).getD (delays.getLast!)
    let resample := (nonnegative input.resample).getD (if interactive then 0 else 1)
    let empty := (nonnegative input.resample).getD 1
    some ⟨delays, resample, empty, input.repair.getD true, input.hops, input.deadline⟩

end Runtime.CompletionConfig
