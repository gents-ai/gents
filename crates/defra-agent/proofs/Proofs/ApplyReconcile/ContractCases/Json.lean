import Proofs.ApplyReconcile.ContractCases.Fixtures

/-! JSON serialization for emitted apply/reconcile contract case witnesses. -/

namespace ApplyReconcile.ContractCases

open Conformance.Contracts

def docRefJson (d : DocRef) : String :=
  "{"
    ++ "\"collection\":" ++ jsonString (collectionName d.collection) ++ ","
    ++ "\"id\":" ++ jsonString d.id
    ++ "}"

def desiredDocJson (entry : ContractDoc) : String :=
  "{"
    ++ "\"collection\":" ++ jsonString (collectionName entry.ref.collection) ++ ","
    ++ "\"id\":" ++ jsonString entry.ref.id ++ ","
    ++ "\"content\":" ++ jsonString entry.content ++ ","
    ++ "\"refs\":"
      ++ jsonArray ((sortedDocRefs entry.refs).map docRefJson)
    ++ "}"

def liveDocJson (entry : ContractLiveDoc) : String :=
  "{"
    ++ "\"collection\":" ++ jsonString (collectionName entry.ref.collection) ++ ","
    ++ "\"id\":" ++ jsonString entry.ref.id ++ ","
    ++ "\"content\":" ++ jsonString entry.content
    ++ "}"

def stepJson (step : ContractStep) : String :=
  "{"
    ++ "\"action\":" ++ jsonString step.action ++ ","
    ++ "\"target\":" ++ docRefJson step.target ++ ","
    ++ "\"content\":" ++ jsonString step.content ++ ","
    ++ "\"refs\":"
      ++ jsonArray ((sortedDocRefs step.refs).map docRefJson)
    ++ "}"

def collectionWriteJson (entry : ContractCollectionWrite) : String :=
  "{"
    ++ "\"collection\":" ++ jsonString (collectionName entry.collection) ++ ","
    ++ "\"graphql_type\":" ++ jsonString entry.graphqlType ++ ","
    ++ "\"unique_field\":" ++ jsonString entry.uniqueField ++ ","
    ++ "\"apply_order\":" ++ toString entry.applyOrder
    ++ "}"

def selectedDocJson (entry : ContractSelectedDoc) : String :=
  "{"
    ++ "\"action\":" ++ jsonString entry.action ++ ","
    ++ "\"target\":" ++ docRefJson entry.target ++ ","
    ++ "\"graphql_type\":" ++ jsonString entry.graphqlType ++ ","
    ++ "\"unique_field\":" ++ jsonString entry.uniqueField ++ ","
    ++ "\"unique_value\":" ++ jsonString entry.uniqueValue ++ ","
    ++ "\"content\":" ++ jsonString entry.content ++ ","
    ++ "\"refs\":"
      ++ jsonArray ((sortedDocRefs entry.refs).map docRefJson)
    ++ "}"

def applyReconcileCaseJson (witness : ApplyReconcileCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"manifest\":" ++ jsonArray (witness.manifest.map desiredDocJson) ++ ","
    ++ "\"pre_desired\":" ++ jsonArray (witness.preDesired.map desiredDocJson) ++ ","
    ++ "\"pre_live\":" ++ jsonArray (witness.preLive.map liveDocJson) ++ ","
    ++ "\"expected_external_state_after_abort\":"
      ++ jsonArray (witness.expectedExternalStateAfterAbort.map liveDocJson) ++ ","
    ++ "\"expected_create\":"
      ++ jsonArray (witness.expectedCreate.map docRefJson) ++ ","
    ++ "\"expected_update\":"
      ++ jsonArray (witness.expectedUpdate.map docRefJson) ++ ","
    ++ "\"expected_unchanged\":"
      ++ jsonArray (witness.expectedUnchanged.map docRefJson) ++ ","
    ++ "\"expected_live_only\":"
      ++ jsonArray (witness.expectedLiveOnly.map docRefJson) ++ ","
    ++ "\"expected_steps\":"
      ++ jsonArray (witness.expectedSteps.map stepJson) ++ ","
    ++ "\"expected_write_order\":"
      ++ jsonArray (witness.expectedWriteOrder.map collectionWriteJson) ++ ","
    ++ "\"expected_selected_create_docs\":"
      ++ jsonArray (witness.expectedSelectedCreateDocs.map selectedDocJson) ++ ","
    ++ "\"expected_selected_update_docs\":"
      ++ jsonArray (witness.expectedSelectedUpdateDocs.map selectedDocJson) ++ ","
    ++ "\"expected_selected_writes\":"
      ++ jsonArray (witness.expectedSelectedWrites.map selectedDocJson) ++ ","
    ++ "\"prefix_len\":" ++ toString witness.prefixLen ++ ","
    ++ "\"expected_prefix_desired\":"
      ++ jsonArray (witness.expectedPrefixDesired.map desiredDocJson) ++ ","
    ++ "\"expected_after_desired\":"
      ++ jsonArray (witness.expectedAfterDesired.map desiredDocJson) ++ ","
    ++ "\"expected_retry_desired\":"
      ++ jsonArray (witness.expectedRetryDesired.map desiredDocJson) ++ ","
    ++ "\"expected_retry_step_count\":"
      ++ toString witness.expectedRetryStepCount ++ ","
    ++ "\"expected_rediff_step_count\":"
      ++ toString witness.expectedRediffStepCount ++ ","
    ++ "\"live_preserved\":" ++ boolString witness.livePreserved ++ ","
    ++ "\"manifest_realized_after\":"
      ++ boolString witness.manifestRealizedAfter ++ ","
    ++ "\"retry_converges\":" ++ boolString witness.retryConverges ++ ","
    ++ "\"idempotent_after\":" ++ boolString witness.idempotentAfter ++ ","
    ++ "\"write_order_prefix_safe\":"
      ++ boolString witness.writeOrderPrefixSafe ++ ","
    ++ "\"production_prefixes_referrers_closed\":"
      ++ boolString witness.productionPrefixesReferrersClosed ++ ","
    ++ "\"prefix_referrers_closed\":"
      ++ boolString witness.prefixReferrersClosed ++ ","
    ++ "\"desired_references_closed_after_prefix\":"
      ++ boolString witness.desiredReferencesClosedAfterPrefix
    ++ "}"

def applyReconcileCasesJson : String :=
  jsonArray (applyReconcileCases.map applyReconcileCaseJson)

end ApplyReconcile.ContractCases
