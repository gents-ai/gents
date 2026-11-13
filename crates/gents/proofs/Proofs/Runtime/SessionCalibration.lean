import Lean
import Proofs.Session.PromptAssembly.ContextCalibration

namespace Runtime.SessionCalibration
open Lean

structure Row where
  accounting : Bool
  matching : Bool
  complete : Bool
  estimated : Nat
  raw : Option Nat
  dispatched : Option Nat
  input : Option Int
  output : Option Int
  cached : Option Int
  created : Option Int
  deriving ToJson

structure Input where
  rows : List Row
  deriving ToJson

structure Anchor where
  inputTokens : Nat
  outputTokens : Nat
  estimatedInputTokens : Nat
  deriving ToJson

structure Observation where
  observations : List Nat
  dispatchedRatio : Option Nat
  anchor : Option Anchor
  deriving ToJson

def counter (value : Option Int) : Option Nat :=
  value.bind fun value => if value < 0 then none else some value.toNat

def observation (row : Row) : Option (Nat × Anchor) := do
  if !row.complete then none else do
    let input ← counter row.input
    let estimated := row.raw.getD row.estimated
    if input < estimated || input < (counter row.cached).getD 0 + (counter row.created).getD 0 then none else do
      let ratio ← PromptAssembly.ContextCalibration.observedRatio input estimated
      some (ratio, ⟨input, (counter row.output).getD 0, estimated⟩)

/-- Only informative calls from the same estimator seed a session. The newest
such call supplies affinity and the prior; older valid calls only fill the
bounded observation window. An absent observation cannot invent an anchor. -/
def seed (input : Input) : Observation :=
  let rows := input.rows.filter fun row => row.accounting && row.matching &&
    ((observation row).isSome || row.dispatched.isSome)
  ⟨((rows.filterMap fun row => (observation row).map Prod.fst).take
      PromptAssembly.ContextCalibration.observationWindow).reverse,
    rows.head?.bind (·.dispatched),
    rows.head?.bind fun row => (observation row).map Prod.snd⟩

end Runtime.SessionCalibration
