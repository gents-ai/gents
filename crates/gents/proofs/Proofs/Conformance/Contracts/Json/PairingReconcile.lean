import Proofs.PairingReconcile
import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

open PairingReconcile

private inductive PairingSample where
  | dial (name : String) (before : ReconcileState) (desired : PairingDesired)
      (desiredMatches : before.desired = some desired)
      (hasWiring : desired.hasWiring = true)
      (disconnected : before.actual.connected = false)
  | reconcileInstallReplicator (name : String) (before : ReconcileState)
      (desired : PairingDesired) (replicator : ReplicatorId)
      (desiredMatches : before.desired = some desired)
      (desiredReplicator : replicator ∈ desired.replicators)
      (missing : replicator ∉ before.actual.replicators)
      (connected : before.actual.connected = true)
  | dialFailed (name : String) (before : ReconcileState) (desired : PairingDesired)
      (desiredMatches : before.desired = some desired)
      (hasWiring : desired.hasWiring = true)
      (disconnected : before.actual.connected = false)

private def PairingSample.name : PairingSample → String
  | .dial name .. | .reconcileInstallReplicator name .. | .dialFailed name .. => name

private def PairingSample.action : PairingSample → String
  | .dial .. => "dial"
  | .reconcileInstallReplicator .. => "reconcileInstallReplicator"
  | .dialFailed .. => "dialFailed"

private def PairingSample.before : PairingSample → ReconcileState
  | .dial _ before .. | .reconcileInstallReplicator _ before .. | .dialFailed _ before .. => before

private def PairingSample.after : PairingSample → ReconcileState
  | .dial _ before .. => dialState before
  | .reconcileInstallReplicator _ before _ replicator .. => installReplicatorState before replicator
  | .dialFailed _ before .. => before

private def PairingSample.legal (sample : PairingSample) :
    Transition sample.before sample.after := by
  cases sample with
  | dial name before desired desiredMatches hasWiring disconnected =>
      exact Transition.dial desired desiredMatches hasWiring disconnected rfl
  | reconcileInstallReplicator name before desired replicator desiredMatches desiredReplicator
      missing connected =>
      exact Transition.reconcileInstallReplicator desired replicator desiredMatches
        desiredReplicator missing connected rfl
  | dialFailed name before desired desiredMatches hasWiring disconnected =>
      exact Transition.dialFailed desired desiredMatches hasWiring disconnected rfl

private def sampleAddresses : List String := ["addr-a", "addr-b"]

private def sampleReplicator (address : String) : ReplicatorId := (address, ∅, ∅)

private def twoReplicators : PairingDesired :=
  { replicators := (sampleAddresses.map sampleReplicator).toFinset }

private def disconnected : ReconcileState :=
  { peer := "peer", desired := some twoReplicators
  , actual := { replicators := ∅, connected := false }
  , applied := { replicators := ∅ }, pairing := [] }

private def connected := dialState disconnected
private def oneInstalled := installReplicatorState connected (sampleReplicator "addr-a")
private def allInstalled := installReplicatorState oneInstalled (sampleReplicator "addr-b")

private def pairingSamples : List PairingSample :=
  [ .dial "dial_leaves_both_replicators_missing" disconnected twoReplicators
      rfl (by decide) rfl
  , .reconcileInstallReplicator "first_install_leaves_second_missing" connected
      twoReplicators (sampleReplicator "addr-a") rfl (by decide) (by decide) rfl
  , .reconcileInstallReplicator "last_install_converges" oneInstalled
      twoReplicators (sampleReplicator "addr-b") rfl (by decide) (by decide) rfl
  , .dialFailed "dial_failure_preserves_missing_resources" disconnected twoReplicators
      rfl (by decide) rfl ]

example : ¬ connected.converged ∧ ¬ oneInstalled.converged ∧ allInstalled.converged := by
  decide

private def pairingStateJson (s : ReconcileState) : String :=
  "{" ++ "\"connected\":" ++ toString s.actual.connected ++ ","
    ++ "\"desired_replicators\":" ++ jsonStringArray
      (sampleAddresses.filter fun address =>
        (s.desired.map (fun d => decide (sampleReplicator address ∈ d.replicators))).getD false) ++ ","
    ++ "\"actual_replicators\":" ++ jsonStringArray
      (sampleAddresses.filter fun address => sampleReplicator address ∈ s.actual.replicators) ++ ","
    ++ "\"converged\":" ++ toString (decide s.converged) ++ "}"

def pairingReconcileCasesJson : String :=
  jsonArray (pairingSamples.map fun sample =>
    "{" ++ "\"name\":" ++ jsonString sample.name ++ ","
      ++ "\"action\":" ++ jsonString sample.action ++ ","
      ++ "\"before\":" ++ pairingStateJson sample.before ++ ","
      ++ "\"after\":" ++ pairingStateJson sample.after ++ "}")

end Conformance.Contracts
