import Proofs.Store.Identity.Operations
import Proofs.Store.Identity.RelationshipReplay
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

structure DeferredCase where
  name : String
  before : DeferredOperation
  runtime : DID
  embeddedRuntime : DID
  nativeOwner : Option DID
  node : Option String
  nodeRegistered : Bool
  committed : Bool

def deferredCases : List DeferredCase :=
  [ ⟨"before-node", beginDeferredOperation, "runtime", "runtime", some "runtime", none, false, true⟩
  , ⟨"success", beginDeferredOperation, "runtime", "runtime", some "runtime", some "node", true, true⟩
  , ⟨"unregistered-node", beginDeferredOperation, "runtime", "runtime", some "runtime", some "node", false, true⟩
  , ⟨"foreign-owner", beginDeferredOperation, "runtime", "runtime", some "other", some "node", true, true⟩
  , ⟨"foreign-runtime", beginDeferredOperation, "runtime", "other", some "runtime", some "node", true, true⟩
  , ⟨"failed-commit", beginDeferredOperation, "runtime", "runtime", some "runtime", some "node", true, false⟩
  , ⟨"revoked-after-publication", ⟨some "node", false, true⟩, "runtime", "runtime", some "runtime", some "node", true, true⟩ ]

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

def settledCases : List (String × Option LiveReconcileStatus × Bool) :=
  [("missing", none, true), ("initial", some ⟨true, false, false⟩, true),
   ("idle-success", some ⟨true, true, false⟩, false),
   ("busy", some ⟨false, true, false⟩, true),
   ("idle-error", some ⟨true, true, true⟩, true)]

structure PairingReaderCase where
  name : String
  before : PairingReaderPublication
  entitled : Bool
  ownerAuthorized : Bool
  committed : Bool
  generation : Nat

def pairingReaderCases : List PairingReaderCase :=
  [ ⟨"initial-grant", ⟨none, false⟩, true, true, true, 1⟩
  , ⟨"not-entitled", ⟨some 1, true⟩, false, true, true, 1⟩
  , ⟨"explicit-revocation", ⟨some 1, false⟩, true, true, true, 1⟩
  , ⟨"new-owner-authorization", ⟨some 1, false⟩, true, true, true, 2⟩
  , ⟨"unauthorized-generation", ⟨some 1, false⟩, true, false, true, 2⟩
  , ⟨"aborted-grant", ⟨none, false⟩, true, true, false, 1⟩
  , ⟨"aborted-revoke", ⟨some 1, true⟩, false, true, false, 1⟩ ]

end Identity.OperationsConformance
