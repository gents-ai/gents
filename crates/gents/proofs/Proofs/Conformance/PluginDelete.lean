import Proofs.Identity.Configuration
import Lean

namespace Conformance.PluginDelete
open Lean

/-- Installed records are supplied by the trusted host after reviewing the
archive. The operations are model inputs, not a client-authored grant. -/
structure Installed where
  coordinate : String
  digest : String
  operations : List String
  deriving FromJson

structure Observation where
  installed : List Installed
  selections : List Identity.Configuration.PluginDeleteSelection
  actor : String
  direct : Bool
  deriving FromJson

def expected (input : Observation) : Bool :=
  let roots := input.installed.filterMap fun record =>
    if record.operations.contains "delete" then
      some (⟨record.coordinate, record.digest⟩ : Identity.Configuration.PluginDeleteRoot)
    else none
  let state : Identity.Configuration.PluginDeleteProjection :=
    { roots := roots, selections := input.selections }
  input.direct || (Identity.Configuration.pluginDeleteSources state).any
    (fun source => source.agent == input.actor && source.permission == "delete-document")

def evaluate (json : Json) : Except String Json := do
  let inputs : Array Observation ← fromJson? json
  pure <| toJson <| inputs.map fun input => expected input

end Conformance.PluginDelete

def main : IO Unit := do
  let input ← (← IO.getStdin).getLine
  match Lean.Json.parse input >>= Conformance.PluginDelete.evaluate with
  | .ok output => IO.println output.compress
  | .error error => throw <| IO.userError error
