import Proofs.Store.PluginStoreHome
import Lean
namespace Conformance.PluginStoreHome
open Lean _root_.PluginStoreHome
deriving instance FromJson, ToJson for Mode, Outcome, Observation, Decision
structure Input where
  mode : Mode
  observation : Observation
  deriving FromJson, ToJson
end Conformance.PluginStoreHome

def main : IO Unit := do
  let line ← (← IO.getStdin).getLine
  match Lean.Json.parse line >>= Lean.fromJson? with
  | .error reason => throw (IO.userError reason)
  | .ok inputs => IO.println ((Lean.toJson
      ((inputs : List Conformance.PluginStoreHome.Input).map fun i =>
        PluginStoreHome.acquire i.mode i.observation)).compress)
