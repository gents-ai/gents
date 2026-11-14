import Proofs.Store.Identity.AgentTargetAccess
import Proofs.Conformance.Family

namespace Conformance.AgentTargetAccess

open Lean Identity.Household Identity.AgentTargetAccess

structure InvokeRow where
  caller : String
  target : String
  invoke : Bool
  deriving ToJson

structure AccessInput where
  state : String
  configurer : String
  enabled : Bool
  targetIds : List String
  jobTargetIds : List String
  deriving ToJson

structure AccessOutput where
  applied : Bool
  invokes : List InvokeRow
  deriving ToJson

def stepResults : List (String × Bool × List InvokeRow) :=
  (sequence.foldl
    (fun (acc : List (String × Bool × List InvokeRow) × State × Config)
          (entry : String × String × Config) =>
      let (out, state, prior) := acc
      let (name, configurer, next) := entry
      let applied := (reconcile state configurer prior next).isSome
      let appliedState := configure state configurer prior next
      (out ++ [(name, applied, pairs.map fun (caller, target) =>
        { caller := caller, target := target, invoke := invokes appliedState caller target })],
        appliedState, if applied then next else prior))
    ([], fixture, unconfigured)).1

@[conformance_family]
def access : Family AccessInput AccessOutput where
  inputs := sequence.map fun entry =>
    let (name, configurer, next) := entry
    let lead := (next.selections.find? (·.agent == "lead")).getD ⟨"lead", false, [], []⟩
    { state := name, configurer := configurer, enabled := lead.enabled,
      targetIds := lead.targetIds, jobTargetIds := lead.jobTargetIds }
  model := fun i =>
    let (_, applied, rows) :=
      (stepResults.find? fun result => result.1 == i.state).getD ("", false, [])
    { applied := applied, invokes := rows }

end Conformance.AgentTargetAccess
