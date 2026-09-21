import Proofs.PairingReconcile
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

open PairingReconcile

/-- Typed sample operations compute their post-state from the same constructor
that supplies the wire label. The premises are exactly those required by the
corresponding modeled `Transition` constructor. -/
private inductive PairingSample where
  | dial (name : String) (before : ReconcileState) (desired : PairingDesired)
      (desiredMatches : before.desired = some desired)
      (hasWiring : desired.hasWiring = true)
      (disconnected : before.actual.connected = false)
  | reconcileInstall (name : String) (before : ReconcileState)
      (desired : PairingDesired) (collection : String)
      (desiredMatches : before.desired = some desired)
      (desiredCollection : collection ∈ desired.collections)
      (missing : collection ∉ before.actual.collections)
      (connected : before.actual.connected = true)
  | dialFailed (name : String) (before : ReconcileState) (desired : PairingDesired)
      (desiredMatches : before.desired = some desired)
      (hasWiring : desired.hasWiring = true)
      (disconnected : before.actual.connected = false)

private def PairingSample.name : PairingSample → String
  | .dial name .. | .reconcileInstall name .. | .dialFailed name .. => name

private def PairingSample.action : PairingSample → String
  | .dial .. => "dial"
  | .reconcileInstall .. => "reconcileInstall"
  | .dialFailed .. => "dialFailed"

private def PairingSample.before : PairingSample → ReconcileState
  | .dial _ before .. | .reconcileInstall _ before .. | .dialFailed _ before .. => before

private def PairingSample.after : PairingSample → ReconcileState
  | .dial _ before .. => dialState before
  | .reconcileInstall _ before _ collection .. => installCollectionState before collection
  | .dialFailed _ before .. => before

private def PairingSample.legal (sample : PairingSample) :
    Transition sample.before sample.after := by
  cases sample with
  | dial name before desired desiredMatches hasWiring disconnected =>
      exact Transition.dial desired desiredMatches hasWiring disconnected rfl
  | reconcileInstall name before desired collection desiredMatches desiredCollection missing connected =>
      exact Transition.reconcileInstall desired collection desiredMatches desiredCollection missing connected rfl
  | dialFailed name before desired desiredMatches hasWiring disconnected =>
      exact Transition.dialFailed desired desiredMatches hasWiring disconnected rfl

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
  [ .dial "dial_leaves_both_collections_missing" disconnected twoCollections
      rfl (by decide) rfl
  , .reconcileInstall "first_install_leaves_second_missing" connected
      twoCollections "AgentContext" rfl (by decide) (by decide) rfl
  , .reconcileInstall "last_install_converges" oneInstalled
      twoCollections "InferenceProfile" rfl (by decide) (by decide) rfl
  , .dialFailed "dial_failure_preserves_missing_resources" disconnected twoCollections
      rfl (by decide) rfl ]

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
