import Proofs.ApplyReconcile.ContractCases.Types
import Proofs.ApplyReconcile.Prefix

/-! Diff, retry, and prefix projections for apply/reconcile contract cases. -/

namespace ApplyReconcile.ContractCases

open Conformance.Contracts

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

def selectedDeleteDocs (steps : List ContractStep) : List ContractSelectedDoc :=
  (productionPruneOrderedSteps steps).filterMap fun step =>
    if step.action == "delete" then some (stepToSelectedDoc step) else none

def createStep (doc : ContractDoc) : ContractStep :=
  { action := "create", target := doc.ref, content := doc.content, refs := doc.refs }

def updateStep (doc : ContractDoc) : ContractStep :=
  { action := "update", target := doc.ref, content := doc.content, refs := doc.refs }

def deleteStep (ref : DocRef) : ContractStep :=
  { action := "delete", target := ref, content := "", refs := [] }

def diffSteps (manifest preDesired : List ContractDoc) : List ContractStep :=
  sortedSteps <|
    manifest.filterMap fun doc =>
      match lookupDoc? preDesired doc.ref with
      | none => some (createStep doc)
      | some live =>
          if desiredDocEq doc live then none else some (updateStep doc)

def docReferencesTarget (doc : ContractDoc) (target : DocRef) : Bool :=
  doc.refs.any fun ref => docRefBEq ref target

def noDesiredDocReferencesTarget (desired : List ContractDoc) (target : DocRef) : Bool :=
  !(desired.any fun doc => docReferencesTarget doc target)

def diffDelete (scenario : ApplyReconcileScenario) : List DocRef :=
  productionPruneOrder.flatMap fun collection =>
    (sortedDocRefs <|
      scenario.preDesired.filterMap fun doc =>
        if (scenario.pruneMode || collection.manifestAuthoritative) &&
            collectionBEq doc.ref.collection collection &&
            !(containsDoc scenario.manifest doc.ref) &&
            noDesiredDocReferencesTarget scenario.preDesired doc.ref then
          some doc.ref
        else
          none)

def diffDeleteSteps (scenario : ApplyReconcileScenario) : List ContractStep :=
  (diffDelete scenario).map deleteStep

def scenarioSteps (scenario : ApplyReconcileScenario) : List ContractStep :=
  diffSteps scenario.manifest scenario.preDesired ++ diffDeleteSteps scenario

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
  if step.action == "delete" then
    sortedDocs <| desired.filter (fun doc => !(docRefBEq doc.ref step.target))
  else
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
    if step.action == "delete" then
      true
    else
      match lookupDoc? desired step.target with
      | some doc => doc.refs.all fun ref => containsDoc desired ref
      | none => false

def adjacentCollectionsPrefixSafe : List Collection → Bool
  | [] => true
  | [_] => true
  | left :: right :: rest =>
      (left.applyOrder <= right.applyOrder) &&
        adjacentCollectionsPrefixSafe (right :: rest)

def adjacentCollectionsPruneSafe : List Collection → Bool
  | [] => true
  | [_] => true
  | left :: right :: rest =>
      (right.applyOrder <= left.applyOrder) &&
        adjacentCollectionsPruneSafe (right :: rest)

def deleteSafetyHolds (preDesired : List ContractDoc) (deleteRefs : List DocRef) : Bool :=
  deleteRefs.all fun target => noDesiredDocReferencesTarget preDesired target

def allProductionPrefixesReferrersClosed
    (preDesired : List ContractDoc)
    (steps : List ContractStep) : Bool :=
  (List.range (steps.length + 1)).all fun prefixLen =>
    let prefixSteps := steps.take prefixLen
    let prefixDesired := applyAll preDesired prefixSteps
    prefixReferrersClosed prefixDesired prefixSteps &&
      desiredReferencesClosed prefixDesired

/-- Contract-case projection for externally visible state after an aborted
    apply prefix. Today this is exactly `preLive`, mirroring the model
    theorems `ApplyReconcile.applyPrefix_preserves_live` and
    `ApplyReconcile.retry_after_prefix_preserves_live`: current apply steps
    have no live-write/delete constructor. #57 should replace this helper
    when external-state mutation semantics become non-trivial. -/
def expectedExternalStateAfterAbort
    (scenario : ApplyReconcileScenario) : List ContractLiveDoc :=
  scenario.preLive

/-- Semantic theorem behind `expectedExternalStateAfterAbort`: in the current
    apply model, aborting any prefix leaves the external/live projection equal
    to the state before the prefix. -/
theorem abort_prefix_preserves_external_state
    {M : Manifest} {L : LiveState} (p : ApplyPrefix M L) :
    p.state.live = L.live :=
  applyPrefix_preserves_live p

def buildCase (scenario : ApplyReconcileScenario) : ApplyReconcileCase :=
  let writeSteps := diffSteps scenario.manifest scenario.preDesired
  let deleteSteps := diffDeleteSteps scenario
  let steps := writeSteps ++ deleteSteps
  let productionSteps := productionOrderedSteps writeSteps
  let prefixSteps := steps.take scenario.prefixLen
  let prefixDesired := applyAll scenario.preDesired prefixSteps
  let after := applyAll scenario.preDesired steps
  let retryScenario := { scenario with preDesired := prefixDesired }
  let retrySteps := scenarioSteps retryScenario
  let retry := applyAll prefixDesired retrySteps
  let rediffScenario := { scenario with preDesired := after }
  let rediff := scenarioSteps rediffScenario
  let reapplied := applyAll after rediff
  { name := scenario.name
  , pruneMode := scenario.pruneMode
  , manifest := scenario.manifest
  , preDesired := scenario.preDesired
  , preLive := scenario.preLive
  , expectedExternalStateAfterAbort := expectedExternalStateAfterAbort scenario
  , expectedCreate := diffCreate scenario
  , expectedUpdate := diffUpdate scenario
  , expectedDelete := diffDelete scenario
  , expectedUnchanged := diffUnchanged scenario
  , expectedLiveOnly := diffLiveOnly scenario
  , expectedSteps := steps
  , expectedWriteOrder := productionWriteOrder.map collectionWriteProjection
  , expectedPruneOrder := productionPruneOrder.map collectionWriteProjection
  , expectedSelectedCreateDocs := selectedDocsByAction "create" writeSteps
  , expectedSelectedUpdateDocs := selectedDocsByAction "update" writeSteps
  , expectedSelectedDeleteDocs := selectedDeleteDocs deleteSteps
  , expectedSelectedWrites := selectedWriteDocs writeSteps
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
  , pruneOrderReferrersBeforeDependencies := adjacentCollectionsPruneSafe productionPruneOrder
  , productionPrefixesReferrersClosed :=
      allProductionPrefixesReferrersClosed scenario.preDesired productionSteps
  , prefixReferrersClosed :=
      prefixReferrersClosed prefixDesired prefixSteps
  , desiredReferencesClosedAfterPrefix := desiredReferencesClosed prefixDesired
  , deleteSafetyHolds := deleteSafetyHolds scenario.preDesired (diffDelete scenario)
  }

/-- Current finite witnesses expose the same external state after abort that
    they started with. This theorem is intentionally small: it names the
    current no-live-write coupling so #57 can update one projection point when
    delete semantics introduce divergent abort expectations. -/
theorem buildCase_expectedExternalStateAfterAbort_eq_preLive
    (scenario : ApplyReconcileScenario) :
    (buildCase scenario).expectedExternalStateAfterAbort = scenario.preLive := by
  rfl

end ApplyReconcile.ContractCases
