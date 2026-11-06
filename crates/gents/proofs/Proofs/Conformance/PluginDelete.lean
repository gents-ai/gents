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

structure Agent where
  nodeDid : String
  agentDid : String
  derived : String
  contextId : String
  enabled : Bool
  deriving FromJson

structure Context where
  nodeDid : String
  docId : String
  contextId : String
  toolsId : String
  deriving FromJson

structure Plugin where
  coordinate : String
  pin : Option String
  deriving FromJson

structure Tools where
  nodeDid : String
  docId : String
  toolsId : String
  plugins : List Plugin
  deriving FromJson

structure Observation where
  node : String
  installed : List Installed
  agents : List Agent
  contexts : List Context
  tools : List Tools
  actor : String
  direct : Bool
  deriving FromJson

/-- Join the observed native document fields in the model; Rust supplies
physical IDs, immutable identity derivation, selected plugin fields, and the
reviewed host records without independently deciding the authorization. These
observations assume well-formed Tools and a ceiling that permits the selected
plugins; the existing ToolPolicy owner models the general ceiling. -/
def selections (input : Observation) : List Identity.Configuration.PluginDeleteSelection :=
  input.agents.flatMap fun agent =>
    input.contexts.flatMap fun context =>
      input.tools.flatMap fun tools =>
        if agent.nodeDid == input.node && context.nodeDid == input.node &&
            tools.nodeDid == input.node && !agent.contextId.isEmpty &&
            agent.contextId == context.contextId && !context.toolsId.isEmpty &&
            context.toolsId == tools.toolsId then
          tools.plugins.map fun plugin =>
            ⟨tools.docId, context.docId, plugin.coordinate, plugin.pin,
              agent.agentDid, agent.derived, agent.enabled⟩
        else []

def expected (input : Observation) : Bool :=
  let roots := input.installed.filterMap fun record =>
    if record.operations.contains "delete" then
      some (⟨record.coordinate, record.digest⟩ : Identity.Configuration.PluginDeleteRoot)
    else none
  let state : Identity.Configuration.PluginDeleteProjection :=
    { roots := roots, selections := selections input }
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
