import Proofs.Runtime.Eval
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.Eval

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

end Conformance.Eval
