import Proofs.ApplyReconcile.Collections
import Proofs.Conformance.ContractTypes

/-!
# Apply/Reconcile Contract Cases

Finite executable apply/reconcile witnesses emitted through
`Proofs.Conformance.Contracts`.

The proof model uses `Manifest.support : Finset DocRef`; `Finset.toList` is a
proof-side representation and has no `lean --run` executable code. These
contract rows therefore use a compact finite list projection over the same
`DocRef`, `Collection.applyOrder`, and create/update step vocabulary. Rust
consumes the emitted inputs and expected outcomes against the production-facing
`apply_model`, so these rows are executable conformance cases rather than a
second Rust-only table.
-/

namespace ApplyReconcile.ContractCases

open Conformance.Contracts

structure ContractDoc where
  ref : DocRef
  content : String
  refs : List DocRef

structure ContractLiveDoc where
  ref : DocRef
  content : String

structure ContractStep where
  action : String
  target : DocRef
  content : String
  refs : List DocRef

structure ContractCollectionWrite where
  collection : Collection
  graphqlType : String
  uniqueField : String
  applyOrder : Nat

structure ContractSelectedDoc where
  action : String
  target : DocRef
  graphqlType : String
  uniqueField : String
  uniqueValue : String
  content : String
  refs : List DocRef

structure ApplyReconcileScenario where
  name : String
  manifest : List ContractDoc
  preDesired : List ContractDoc
  preLive : List ContractLiveDoc
  prefixLen : Nat

structure ApplyReconcileCase where
  name : String
  manifest : List ContractDoc
  preDesired : List ContractDoc
  preLive : List ContractLiveDoc
  expectedCreate : List DocRef
  expectedUpdate : List DocRef
  expectedUnchanged : List DocRef
  expectedLiveOnly : List DocRef
  expectedSteps : List ContractStep
  expectedWriteOrder : List ContractCollectionWrite
  expectedSelectedCreateDocs : List ContractSelectedDoc
  expectedSelectedUpdateDocs : List ContractSelectedDoc
  expectedSelectedWrites : List ContractSelectedDoc
  prefixLen : Nat
  expectedPrefixDesired : List ContractDoc
  expectedAfterDesired : List ContractDoc
  expectedRetryDesired : List ContractDoc
  expectedRetryStepCount : Nat
  expectedRediffStepCount : Nat
  livePreserved : Bool
  manifestRealizedAfter : Bool
  retryConverges : Bool
  idempotentAfter : Bool
  writeOrderPrefixSafe : Bool
  productionPrefixesReferrersClosed : Bool
  prefixReferrersClosed : Bool
  desiredReferencesClosedAfterPrefix : Bool

def boolString (value : Bool) : String :=
  if value then "true" else "false"

def collectionName : Collection → String
  | .agentPrincipal => "AgentPrincipal"
  | .agentBehavior => "AgentBehavior"
  | .toolSelection => "ToolSelection"
  | .inferenceBackend => "InferenceBackend"
  | .inferenceProfile => "InferenceProfile"
  | .toolServiceRegistry => "ToolServiceRegistry"
  | .task => "Task"
  | .schedule => "Schedule"
  | .eventTrigger => "EventTrigger"

def collectionUniqueField : Collection → String
  | .agentPrincipal => "agent_did"
  | .agentBehavior => "behavior_id"
  | .toolSelection => "selection_id"
  | .inferenceBackend => "backend_id"
  | .inferenceProfile => "profile_id"
  | .toolServiceRegistry => "service_id"
  | .task => "task_id"
  | .schedule => "schedule_id"
  | .eventTrigger => "trigger_id"

def productionWriteOrder : List Collection :=
  [ .inferenceBackend
  , .inferenceProfile
  , .toolServiceRegistry
  , .toolSelection
  , .agentBehavior
  , .task
  , .schedule
  , .eventTrigger
  , .agentPrincipal
  ]

def collectionWriteProjection (collection : Collection) : ContractCollectionWrite :=
  { collection := collection
  , graphqlType := collectionName collection
  , uniqueField := collectionUniqueField collection
  , applyOrder := collection.applyOrder
  }

def collectionBEq (a b : Collection) : Bool :=
  if a = b then true else false

def docRefBEq (a b : DocRef) : Bool :=
  if a = b then true else false

def docRefLt (a b : DocRef) : Bool :=
  if a.collection.applyOrder < b.collection.applyOrder then true
  else if b.collection.applyOrder < a.collection.applyOrder then false
  else a.id < b.id

def docRefLe (a b : DocRef) : Bool :=
  docRefLt a b || docRefBEq a b

def sortedDocRefs (refs : List DocRef) : List DocRef :=
  refs.mergeSort docRefLe

def docRefsEq (left right : List DocRef) : Bool :=
  let left := sortedDocRefs left
  let right := sortedDocRefs right
  left.length == right.length &&
    ((left.zip right).all (fun pair => docRefBEq pair.fst pair.snd))

def desiredDocEq (left right : ContractDoc) : Bool :=
  left.content == right.content && docRefsEq left.refs right.refs

def lookupDoc? (docs : List ContractDoc) (ref : DocRef) : Option ContractDoc :=
  docs.find? (fun doc => docRefBEq doc.ref ref)

def containsDoc (docs : List ContractDoc) (ref : DocRef) : Bool :=
  (lookupDoc? docs ref).isSome

def contractDocLe (a b : ContractDoc) : Bool :=
  docRefLe a.ref b.ref

def contractStepLe (a b : ContractStep) : Bool :=
  docRefLe a.target b.target

def sortedDocs (docs : List ContractDoc) : List ContractDoc :=
  docs.mergeSort contractDocLe

def sortedSteps (steps : List ContractStep) : List ContractStep :=
  steps.mergeSort contractStepLe

def productionOrderedSteps (steps : List ContractStep) : List ContractStep :=
  productionWriteOrder.flatMap fun collection =>
    steps.filter fun step => collectionBEq step.target.collection collection

def stepToDoc (step : ContractStep) : ContractDoc :=
  { ref := step.target, content := step.content, refs := step.refs }

def stepToSelectedDoc (step : ContractStep) : ContractSelectedDoc :=
  { action := step.action
  , target := step.target
  , graphqlType := collectionName step.target.collection
  , uniqueField := collectionUniqueField step.target.collection
  , uniqueValue := step.target.id
  , content := step.content
  , refs := step.refs
  }

def selectedDocsByAction
    (action : String)
    (steps : List ContractStep) : List ContractSelectedDoc :=
  (productionOrderedSteps steps).filterMap fun step =>
    if step.action == action then some (stepToSelectedDoc step) else none

def selectedWriteDocs (steps : List ContractStep) : List ContractSelectedDoc :=
  (productionOrderedSteps steps).map stepToSelectedDoc

def createStep (doc : ContractDoc) : ContractStep :=
  { action := "create", target := doc.ref, content := doc.content, refs := doc.refs }

def updateStep (doc : ContractDoc) : ContractStep :=
  { action := "update", target := doc.ref, content := doc.content, refs := doc.refs }

def diffSteps (manifest preDesired : List ContractDoc) : List ContractStep :=
  sortedSteps <|
    manifest.filterMap fun doc =>
      match lookupDoc? preDesired doc.ref with
      | none => some (createStep doc)
      | some live =>
          if desiredDocEq doc live then none else some (updateStep doc)

def diffCreate (scenario : ApplyReconcileScenario) : List DocRef :=
  sortedDocRefs <|
    scenario.manifest.filterMap fun doc =>
      match lookupDoc? scenario.preDesired doc.ref with
      | none => some doc.ref
      | some _ => none

def diffUpdate (scenario : ApplyReconcileScenario) : List DocRef :=
  sortedDocRefs <|
    scenario.manifest.filterMap fun doc =>
      match lookupDoc? scenario.preDesired doc.ref with
      | some live =>
          if desiredDocEq doc live then none else some doc.ref
      | none => none

def diffUnchanged (scenario : ApplyReconcileScenario) : List DocRef :=
  sortedDocRefs <|
    scenario.manifest.filterMap fun doc =>
      match lookupDoc? scenario.preDesired doc.ref with
      | some live =>
          if desiredDocEq doc live then some doc.ref else none
      | none => none

def diffLiveOnly (scenario : ApplyReconcileScenario) : List DocRef :=
  sortedDocRefs <|
    scenario.preDesired.filterMap fun doc =>
      match lookupDoc? scenario.manifest doc.ref with
      | some _ => none
      | none => some doc.ref

def applyOne (desired : List ContractDoc) (step : ContractStep) : List ContractDoc :=
  sortedDocs <| stepToDoc step ::
    desired.filter (fun doc => !(docRefBEq doc.ref step.target))

def applyAll (desired : List ContractDoc) (steps : List ContractStep) : List ContractDoc :=
  steps.foldl applyOne desired

def desiredDocsEq (left right : List ContractDoc) : Bool :=
  let left := sortedDocs left
  let right := sortedDocs right
  left.length == right.length &&
    ((left.zip right).all (fun pair =>
      docRefBEq pair.fst.ref pair.snd.ref &&
        desiredDocEq pair.fst pair.snd))

def manifestRealizedBool (manifest desired : List ContractDoc) : Bool :=
  manifest.all fun doc =>
    match lookupDoc? desired doc.ref with
    | some actual => desiredDocEq doc actual
    | none => false

def desiredReferencesClosed (desired : List ContractDoc) : Bool :=
  desired.all fun doc =>
    doc.refs.all fun ref => containsDoc desired ref

def prefixReferrersClosed
    (desired : List ContractDoc)
    (steps : List ContractStep) : Bool :=
  steps.all fun step =>
    match lookupDoc? desired step.target with
    | some doc => doc.refs.all fun ref => containsDoc desired ref
    | none => false

def adjacentCollectionsPrefixSafe : List Collection → Bool
  | [] => true
  | [_] => true
  | left :: right :: rest =>
      (left.applyOrder <= right.applyOrder) &&
        adjacentCollectionsPrefixSafe (right :: rest)

def allProductionPrefixesReferrersClosed
    (preDesired : List ContractDoc)
    (steps : List ContractStep) : Bool :=
  (List.range (steps.length + 1)).all fun prefixLen =>
    let prefixSteps := steps.take prefixLen
    let prefixDesired := applyAll preDesired prefixSteps
    prefixReferrersClosed prefixDesired prefixSteps &&
      desiredReferencesClosed prefixDesired

def buildCase (scenario : ApplyReconcileScenario) : ApplyReconcileCase :=
  let steps := diffSteps scenario.manifest scenario.preDesired
  let productionSteps := productionOrderedSteps steps
  let prefixSteps := steps.take scenario.prefixLen
  let prefixDesired := applyAll scenario.preDesired prefixSteps
  let after := applyAll scenario.preDesired steps
  let retrySteps := diffSteps scenario.manifest prefixDesired
  let retry := applyAll prefixDesired retrySteps
  let rediff := diffSteps scenario.manifest after
  let reapplied := applyAll after rediff
  { name := scenario.name
  , manifest := scenario.manifest
  , preDesired := scenario.preDesired
  , preLive := scenario.preLive
  , expectedCreate := diffCreate scenario
  , expectedUpdate := diffUpdate scenario
  , expectedUnchanged := diffUnchanged scenario
  , expectedLiveOnly := diffLiveOnly scenario
  , expectedSteps := steps
  , expectedWriteOrder := productionWriteOrder.map collectionWriteProjection
  , expectedSelectedCreateDocs := selectedDocsByAction "create" steps
  , expectedSelectedUpdateDocs := selectedDocsByAction "update" steps
  , expectedSelectedWrites := selectedWriteDocs steps
  , prefixLen := scenario.prefixLen
  , expectedPrefixDesired := sortedDocs prefixDesired
  , expectedAfterDesired := sortedDocs after
  , expectedRetryDesired := sortedDocs retry
  , expectedRetryStepCount := retrySteps.length
  , expectedRediffStepCount := rediff.length
  -- The list projection has no live-write constructor; Rust checks this
  -- invariant against `apply_model::apply_all` using the emitted pre-live rows.
  , livePreserved := true
  , manifestRealizedAfter := manifestRealizedBool scenario.manifest after
  , retryConverges := desiredDocsEq retry after
  , idempotentAfter := desiredDocsEq reapplied after
  , writeOrderPrefixSafe := adjacentCollectionsPrefixSafe productionWriteOrder
  , productionPrefixesReferrersClosed :=
      allProductionPrefixesReferrersClosed scenario.preDesired productionSteps
  , prefixReferrersClosed :=
      prefixReferrersClosed prefixDesired prefixSteps
  , desiredReferencesClosedAfterPrefix := desiredReferencesClosed prefixDesired
  }

def doc (collection : Collection) (id : String) : DocRef :=
  { collection := collection, id := id }

def desired
    (collection : Collection)
    (id content : String)
    (refs : List DocRef := []) : ContractDoc :=
  { ref := doc collection id, content := content, refs := refs }

def live
    (collection : Collection)
    (id content : String) : ContractLiveDoc :=
  { ref := doc collection id, content := content }

def backendA : DocRef := doc .inferenceBackend "backend-a"
def backendB : DocRef := doc .inferenceBackend "backend-b"
def selectionA : DocRef := doc .toolSelection "selection-a"
def profileA : DocRef := doc .inferenceProfile "profile-a"
def serviceA : DocRef := doc .toolServiceRegistry "service-a"
def behaviorA : DocRef := doc .agentBehavior "behavior-a"
def taskA : DocRef := doc .task "task-a"
def scheduleA : DocRef := doc .schedule "schedule-a"
def eventTriggerA : DocRef := doc .eventTrigger "trigger-a"
def principalA : DocRef := doc .agentPrincipal "did:example:agent"

def applyReconcileScenarios : List ApplyReconcileScenario :=
  [ { name := "empty_manifest"
    , manifest := []
    , preDesired := []
    , preLive := []
    , prefixLen := 0
    }
  , { name := "backend_before_behavior_ordering"
    , manifest :=
        [ desired .agentBehavior "behavior-a" "behavior-desired" [backendA]
        , desired .inferenceBackend "backend-a" "backend-desired"
        ]
    , preDesired := []
    , preLive := []
    , prefixLen := 0
    }
  , { name := "update_existing_backend"
    , manifest := [desired .inferenceBackend "backend-a" "backend-new"]
    , preDesired := [desired .inferenceBackend "backend-a" "backend-old"]
    , preLive := [live .inferenceBackend "backend-a" "runtime-probe"]
    , prefixLen := 0
    }
  , { name := "live_only_no_op"
    , manifest := []
    , preDesired := [desired .inferenceBackend "backend-b" "orphan-desired"]
    , preLive := [live .inferenceBackend "backend-b" "orphan-runtime"]
    , prefixLen := 0
    }
  , { name := "prefix_retry_convergence_idempotence"
    , manifest :=
        [ desired .task "task-a" "task-desired" [behaviorA]
        , desired .agentBehavior "behavior-a" "behavior-desired" [backendA]
        , desired .inferenceBackend "backend-a" "backend-desired"
        ]
    , preDesired := []
    , preLive := [live .agentBehavior "behavior-a" "runtime-live"]
    , prefixLen := 1
    }
  , { name := "referrer_closure"
    , manifest :=
        [ desired .agentPrincipal "did:example:agent" "principal-desired" [behaviorA]
        , desired .task "task-a" "task-desired" [behaviorA]
        , desired .agentBehavior "behavior-a" "behavior-desired"
            [backendA, selectionA, profileA]
        , desired .toolSelection "selection-a" "selection-desired"
        , desired .inferenceProfile "profile-a" "profile-desired"
        , desired .inferenceBackend "backend-a" "backend-desired"
        ]
    , preDesired := []
    , preLive := []
    , prefixLen := 4
    }
  , { name := "production_write_boundary_all_collections"
    , manifest :=
        [ desired .inferenceBackend "backend-a" "backend-desired"
        , desired .inferenceProfile "profile-a" "profile-desired"
        , desired .toolServiceRegistry "service-a" "service-desired"
        , desired .toolSelection "selection-a" "selection-desired"
        , desired .agentBehavior "behavior-a" "behavior-desired"
            [backendA, selectionA, profileA, serviceA]
        , desired .task "task-a" "task-desired" [behaviorA]
        , desired .schedule "schedule-a" "schedule-desired"
        , desired .eventTrigger "trigger-a" "trigger-desired" [taskA]
        , desired .agentPrincipal "did:example:agent" "principal-desired" [behaviorA]
        ]
    , preDesired := []
    , preLive := []
    , prefixLen := 6
    }
  ]

def applyReconcileCases : List ApplyReconcileCase :=
  applyReconcileScenarios.map buildCase

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
