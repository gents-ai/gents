import Proofs.P2P.PairingReconcile
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Lean PairingReconcile

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

structure PairingStateWire where
  connected : Bool
  desiredReplicators : List String
  actualReplicators : List String
  converged : Bool
  deriving ToJson, BEq

def PairingStateWire.ofModel (s : ReconcileState) : PairingStateWire :=
  { connected := s.actual.connected
    desiredReplicators := sampleAddresses.filter fun address =>
      (s.desired.map (fun d => decide (sampleReplicator address ∈ d.replicators))).getD false
    actualReplicators :=
      sampleAddresses.filter fun address => sampleReplicator address ∈ s.actual.replicators
    converged := decide s.converged }

/-- The replicators a state still lacks, in address order. -/
def pairingPending (s : ReconcileState) : List String :=
  sampleAddresses.filter fun address =>
    (s.desired.map (fun d => decide (sampleReplicator address ∈ d.replicators))).getD false &&
      !decide (sampleReplicator address ∈ s.actual.replicators)

structure PairingSampleInput where
  name : String
  action : String
  before : PairingStateWire
  after : PairingStateWire
  deriving ToJson

structure PairingSampleProjection where
  beforePending : List String
  afterPending : List String
  deriving ToJson

private def PairingSampleInput.ofSample (sample : PairingSample) : PairingSampleInput :=
  { name := sample.name, action := sample.action
    before := PairingStateWire.ofModel sample.before, after := PairingStateWire.ofModel sample.after }

/-- Each sample is a legal transition of the named sample; a sample whose
states differ from that transition projects nothing. -/
@[conformance_family]
def pairingReconcileProjection : Conformance.Family PairingSampleInput (Option PairingSampleProjection) where
  inputs := pairingSamples.map PairingSampleInput.ofSample
  model := fun i => (pairingSamples.find? (·.name == i.name)).bind fun sample =>
    if (PairingSampleInput.ofSample sample).before == i.before &&
        (PairingSampleInput.ofSample sample).after == i.after then
      some { beforePending := pairingPending sample.before
             afterPending := pairingPending sample.after }
    else none

private def sampleWellFormed (sample : PairingSample) : Bool :=
  let before := PairingStateWire.ofModel sample.before
  let after := PairingStateWire.ofModel sample.after
  before.desiredReplicators.length ≥ 2 && before.desiredReplicators == after.desiredReplicators &&
    match sample.action with
    | "dial" => !before.connected && after.connected &&
        before.actualReplicators == after.actualReplicators
    | "dialFailed" => !before.connected && !after.connected &&
        before.actualReplicators == after.actualReplicators
    | _ => (after.actualReplicators.filter (!before.actualReplicators.contains ·)).length == 1 &&
        pairingPending sample.before == (pairingPending sample.before).take 1 ++ pairingPending sample.after

example : pairingSamples.all sampleWellFormed = true := by native_decide

example : (pairingSamples.map (·.name)).Nodup := by decide

example : pairingSamples.any (fun s => s.after.actual.connected && !decide s.after.converged) ∧
    pairingSamples.any (fun s => decide s.after.converged && (pairingPending s.after).isEmpty) := by
  native_decide

end Conformance.Contracts
