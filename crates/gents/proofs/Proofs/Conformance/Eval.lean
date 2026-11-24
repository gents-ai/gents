import Proofs.Runtime.Eval
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.Eval

open Lean
open Conformance.Contracts
open _root_.Eval

deriving instance Lean.ToJson for OutcomeKind, ProviderReason, EvidenceClass

structure OutcomeInput where
  kind : OutcomeKind
  providerReason : Option ProviderReason
  deriving Lean.ToJson

def outcomeInputs : List OutcomeInput :=
  allKinds.flatMap fun kind =>
    ([none] ++ allReasons.map some).map fun providerReason => { kind, providerReason }

@[conformance_family]
def classification : Conformance.Family OutcomeInput EvidenceClass where
  inputs := outcomeInputs
  model := fun i => classify i.kind i.providerReason

@[conformance_family]
def subjectCausability : Conformance.Family OutcomeInput Bool where
  inputs := outcomeInputs
  model := fun i => subjectCausable i.kind i.providerReason

structure CaptureInput where
  collection : String
  fields : List String
  filterFields : List String
  /-- The capture filter as JSON text: one `{"_eq":"$trial"}` clause per filter field,
  wrapped in `_or` when nested. -/
  filter : String
  deriving Lean.ToJson

def captureFilter (filterFields : List String) (nested : Bool) : String :=
  let filter := "{" ++ String.intercalate "," (filterFields.map fun field =>
    jsonString field ++ ":{\"_eq\":\"$trial\"}") ++ "}"
  if nested then "{\"_or\":[" ++ filter ++ "]}" else filter

@[conformance_family]
def documentCapture : Conformance.Family CaptureInput Bool where
  inputs :=
    (["Task", "Agent"] ++ ScopeTemplates.credentialCollections).flatMap fun collection =>
      [[], ["backend_id"], ["backend_id", "endpoint", "enabled", "max_concurrent"],
        ["auth"], ["backend_id", "auth"], ["credential_id"], ["access_token"],
        ["refresh_token"], ["id_token"]].flatMap fun fields =>
          [[], ["node_did"], ["auth"], ["node_did", "auth"], ["access_token"],
            ["refresh_token"], ["id_token"]].flatMap fun filterFields =>
              [false, true].map fun nested =>
                { collection, fields, filterFields, filter := captureFilter filterFields nested }
  model := fun i => documentCaptureAllowed i.collection i.fields i.filterFields

deriving instance ToJson for ReasonRecord

structure StoredVerdictInput where
  kind : OutcomeKind
  providerReason : Option ProviderReason
  score : Option Nat
  deriving ToJson

structure StoredVerdictOutput where
  score : Option Nat
  reason : ReasonRecord
  deriving ToJson

@[conformance_family]
def storedVerdict : Family StoredVerdictInput StoredVerdictOutput where
  inputs := allKinds.flatMap fun kind =>
    ([none] ++ allReasons.map some).flatMap fun reason =>
      [none, some 0, some 10000].map fun score => { kind, providerReason := reason, score }
  model := fun i =>
    { score := storedScore i.kind i.providerReason i.score
      reason := recordedReason i.providerReason }

deriving instance ToJson for FireFailure, SeedObservation, SeedAction, SeedStep,
  FiredRun, RunSelector

def failureLists : List (List FireFailure) :=
  [[], [.error], [.budgetExhausted], [.unacknowledged], [.unacknowledged, .unacknowledged],
    [.unacknowledged, .error], [.error, .unacknowledged], [.budgetExhausted, .unacknowledged]]

structure StageOutcomeInput where
  request : Option OutcomeKind
  triggersRead : Bool
  failures : List FireFailure
  deriving ToJson

@[conformance_family]
def stageOutcomes : Family StageOutcomeInput (Option OutcomeKind) where
  inputs := ([none] ++ allKinds.map some).flatMap fun request =>
    [false, true].flatMap fun triggersRead =>
      failureLists.map fun failures => { request, triggersRead, failures }
  model := fun i => stageOutcome i.request i.triggersRead i.failures

@[conformance_family]
def seedSteps : Family SeedObservation SeedStep where
  inputs := [true, false].flatMap fun expectFire =>
    [true, false].flatMap fun observable =>
      [false, true].flatMap fun fired =>
        failureLists.flatMap fun failures =>
          [false, true].flatMap fun quiet =>
            [false, true].map fun windowEnded =>
              { expectFire, observable, fired, failures, quiet, windowEnded }
  model := seedStep

structure RunAddressingInput where
  selector : RunSelector
  seed : Option String
  run : FiredRun
  deriving ToJson

@[conformance_family]
def runAddressing : Family RunAddressingInput Bool where
  inputs :=
    [⟨none, none, false⟩, ⟨some "pack-label", none, false⟩, ⟨none, some "PackedOrder", false⟩,
      ⟨none, none, true⟩, ⟨some "pack-label", some "PackedOrder", true⟩].flatMap fun selector =>
      [none, some "seed-1"].flatMap fun seed =>
        [none, some "pack-label", some "chain-start"].flatMap fun trigger =>
          [none, some "PackedOrder", some "Order"].flatMap fun watched =>
            [none, some "seed-1", some "row-7"].flatMap fun source =>
              [false, true].map fun priorToStage =>
                { selector, seed, run := { trigger, watched, source, priorToStage } }
  model := fun i => runAddressed i.selector i.seed i.run

end Conformance.Eval
