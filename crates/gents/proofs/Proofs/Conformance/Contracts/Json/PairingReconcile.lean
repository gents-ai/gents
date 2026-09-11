import Proofs.PairingReconcile
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

open PairingReconcile

/-- Samples execute the transport owner's operations and carry its transition proof.
Convergence depends on all desired resources, not an independent phase machine. -/
private structure PairingSample where
  name : String
  action : String
  before : ReconcileState
  after : ReconcileState
  legal : Transition before after

private def twoCollections : PairingDesired :=
  { collections := ["AgentContext", "InferenceProfile"].toFinset, replicators := ∅ }

private def disconnected : ReconcileState :=
  { peer := "peer", desired := some twoCollections
  , actual := { collections := ∅, replicators := ∅, connected := false }
  , applied := { collections := ∅, replicators := ∅ }, pairing := [] }

private def connected := dialState disconnected
private def oneInstalled := installCollectionState connected "AgentContext"
private def allInstalled := installCollectionState oneInstalled "InferenceProfile"

private def pairingSamples : List PairingSample :=
  [ { name := "dial_leaves_both_collections_missing", action := "dial"
    , before := disconnected, after := connected
    , legal := Transition.dial twoCollections rfl (by decide) rfl rfl }
  , { name := "first_install_leaves_second_missing", action := "reconcileInstall"
    , before := connected, after := oneInstalled
    , legal := Transition.reconcileInstall twoCollections "AgentContext" rfl
        (by decide) (by decide) rfl rfl }
  , { name := "last_install_converges", action := "reconcileInstall"
    , before := oneInstalled, after := allInstalled
    , legal := Transition.reconcileInstall twoCollections "InferenceProfile" rfl
        (by decide) (by decide) rfl rfl }
  , { name := "dial_failure_preserves_missing_resources", action := "dialFailed"
    , before := disconnected, after := disconnected
    , legal := Transition.dialFailed twoCollections rfl (by decide) rfl rfl } ]

example : ¬ connected.converged ∧ ¬ oneInstalled.converged ∧ allInstalled.converged := by
  decide

private def pairingStateJson (s : ReconcileState) : String :=
  "{" ++ "\"connected\":" ++ toString s.actual.connected ++ ","
    ++ "\"desired_collections\":" ++ jsonStringArray
      (["AgentContext", "InferenceProfile"].filter fun c =>
        (s.desired.map (fun d => decide (c ∈ d.collections))).getD false) ++ ","
    ++ "\"actual_collections\":" ++ jsonStringArray (["AgentContext", "InferenceProfile"].filter fun c => c ∈ s.actual.collections) ++ ","
    ++ "\"converged\":" ++ toString (decide s.converged) ++ "}"

def pairingReconcileCasesJson : String :=
  jsonArray (pairingSamples.map fun sample =>
    "{" ++ "\"name\":" ++ jsonString sample.name ++ ","
      ++ "\"action\":" ++ jsonString sample.action ++ ","
      ++ "\"before\":" ++ pairingStateJson sample.before ++ ","
      ++ "\"after\":" ++ pairingStateJson sample.after ++ "}")

end Conformance.Contracts
