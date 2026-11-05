import Proofs.Identity.Operations
import Lean

namespace Identity.OperationsConformance
open Lean Identity.Household Identity.Operations

def bindingCases : List (String × CollectionBinding) :=
  [("absent", .absent), ("expected", .expected), ("unbound", .unbound),
   ("different", .different)]

structure RecoveryCase where
  name : String
  pending : Bool
  parentGranted : Bool
  runtime : String
  nativeOwner : Option String
  recordedParent : Option String
  currentParent : Option String
  grantSucceeded : Bool
  markSucceeded : Bool
  embeddedRuntime : String
  parentRegistered : Bool

def recoveryCases : List RecoveryCase :=
  [ ⟨"success", true, false, "runtime", some "runtime", some "node", some "node", true, true, "runtime", true⟩
  , ⟨"grant-failed", true, false, "runtime", some "runtime", some "node", some "node", false, true, "runtime", true⟩
  , ⟨"mark-failed", true, false, "runtime", some "runtime", some "node", some "node", true, false, "runtime", true⟩
  , ⟨"retry-mark", true, true, "runtime", some "runtime", some "node", some "node", false, true, "runtime", true⟩
  , ⟨"owner-unknown", true, false, "runtime", none, some "node", some "node", true, true, "runtime", true⟩
  , ⟨"owner-mismatch", true, false, "runtime", some "other", some "node", some "node", true, true, "runtime", true⟩
  , ⟨"runtime-missing", true, false, "", some "", some "node", some "node", true, true, "runtime", true⟩
  , ⟨"parent-missing", true, false, "runtime", some "runtime", none, none, true, true, "runtime", true⟩
  , ⟨"node-unknown", true, false, "runtime", some "runtime", some "node", none, true, true, "runtime", true⟩
  , ⟨"parent-mismatch", true, false, "runtime", some "runtime", some "node", some "other", true, true, "runtime", true⟩
  , ⟨"foreign-runtime", true, false, "runtime", some "runtime", some "node", some "node", true, true, "other", true⟩
  , ⟨"unregistered-parent", true, false, "runtime", some "runtime", some "node", some "node", true, true, "runtime", false⟩
  , ⟨"already-complete", false, false, "runtime", some "runtime", some "node", some "node", true, true, "runtime", true⟩ ]

def recoveryCaseJson (c : RecoveryCase) : Json :=
  let before : ParentPublication := ⟨true, c.parentGranted, !c.pending⟩
  let after := repairReadiness before c.runtime c.embeddedRuntime c.parentRegistered c.nativeOwner c.recordedParent c.currentParent
    c.grantSucceeded c.markSucceeded
  Json.mkObj [("name", toJson c.name), ("pending", toJson c.pending),
    ("parent_granted", toJson c.parentGranted), ("runtime", toJson c.runtime),
    ("embedded_runtime", toJson c.embeddedRuntime), ("parent_registered", toJson c.parentRegistered),
    ("native_owner", toJson c.nativeOwner), ("recorded_parent", toJson c.recordedParent),
    ("current_parent", toJson c.currentParent), ("grant_succeeded", toJson c.grantSucceeded),
    ("mark_succeeded", toJson c.markSucceeded), ("expected_parent_granted", toJson after.parentGranted),
    ("expected_parent_linked", toJson after.parentLinked)]

def preflightCasesJson : String := (toJson (bindingCases.map fun (name, binding) =>
  Json.mkObj [("name", toJson name), ("expected", toJson (readinessPreflight binding))])).compress

def recoveryCasesJson : String := (toJson (recoveryCases.map recoveryCaseJson)).compress

def validObservation : ReadinessObservation :=
  ⟨"runtime", "runtime", 1, 1, some "node", some "user", some "runtime", some "node", true, true⟩

def observationCases : List (String × ReadinessObservation) :=
  [("valid", validObservation),
   ("foreign-runtime", { validObservation with embeddedRuntime := "other" }),
   ("no-node", { validObservation with nodeCount := 0 }),
   ("duplicate-node", { validObservation with nodeCount := 2 }),
   ("no-readiness", { validObservation with readinessCount := 0 }),
   ("duplicate-readiness", { validObservation with readinessCount := 2 }),
   ("node-unregistered", { validObservation with nodeOwner := none }),
   ("readiness-unregistered", { validObservation with readinessOwner := none }),
   ("foreign-readiness", { validObservation with readinessOwner := some "other" }),
   ("missing-parent", { validObservation with recordedParent := none }),
   ("replaced-parent", { validObservation with nodeId := some "replacement" }),
   ("pending", { validObservation with parentLinked := false }),
   ("revoked-after-publication", { validObservation with parentPresent := false })]

def observationCaseJson (name : String) (o : ReadinessObservation) : Json := Json.mkObj
  [("name", toJson name), ("requested_runtime", toJson o.requestedRuntime),
   ("embedded_runtime", toJson o.embeddedRuntime), ("node_count", toJson o.nodeCount),
   ("readiness_count", toJson o.readinessCount), ("node_id", toJson o.nodeId),
   ("node_owner", toJson o.nodeOwner), ("readiness_owner", toJson o.readinessOwner),
   ("recorded_parent", toJson o.recordedParent), ("parent_linked", toJson o.parentLinked),
   ("parent_present", toJson o.parentPresent), ("expected_usable", toJson (readinessObservationUsable o))]

def observationCasesJson : String := (toJson (observationCases.map fun (name, value) =>
  observationCaseJson name value)).compress

def settledCases : List (String × Option LiveReconcileStatus × Bool) :=
  [("missing", none, true), ("initial", some ⟨true, false, false⟩, true),
   ("idle-success", some ⟨true, true, false⟩, false),
   ("busy", some ⟨false, true, false⟩, true),
   ("idle-error", some ⟨true, true, true⟩, true)]

def settledCasesJson : String := (toJson (settledCases.map fun (name, status, persisted) =>
  Json.mkObj [("name", toJson name), ("live_present", toJson status.isSome),
    ("idle", toJson (status.any (fun (s : LiveReconcileStatus) => s.idle))),
    ("result_known", toJson (status.any (fun (s : LiveReconcileStatus) => s.lastResultKnown))),
    ("result_error", toJson (status.any (fun (s : LiveReconcileStatus) => s.lastResultError))),
    ("persisted_settled", toJson persisted),
    ("expected", toJson (goalRuntimeSettled status persisted))])).compress

end Identity.OperationsConformance
