import Proofs.Store.StorageFailure
import Proofs.Conformance.Family

namespace Conformance.StorageFailureContracts

open Lean

structure FailureInput where
  code : String
  message : String
  deriving ToJson

/-- Unknown codes and misleading messages must not acquire storage authority. -/
def failures : List FailureInput :=
  [ { code := "STORAGE_FULL", message := "localized storage failure" }
  , { code := "STORAGE_FULL", message := "please retry" }
  , { code := "", message := "No space left on device (os error 28)" }
  , { code := "OTHER", message := "STORAGE_FULL" } ]

structure ClassificationOutput where
  full : Bool
  deriving ToJson

@[conformance_family]
def classification : Family FailureInput ClassificationOutput where
  inputs := failures
  model := fun i => { full := StorageFailure.isFull i.code }

structure ReadinessOutput where
  fatalReadiness : Bool
  deriving ToJson

@[conformance_family]
def readiness : Family FailureInput ReadinessOutput where
  inputs := failures
  model := fun i => { fatalReadiness := StorageFailure.fatalReadiness i.code }

structure RetryOutput where
  stopAutomaticRetry : Bool
  deriving ToJson

/-- The stop is observed on a write that would otherwise be retried: the
failure arrives beside a store conflict, so only the storage code can stop the
retry. -/
@[conformance_family]
def ambiguousWrite : Family FailureInput RetryOutput where
  inputs := failures
  model := fun i => { stopAutomaticRetry := StorageFailure.stopAutomaticRetry i.code }

structure TransactionOutput where
  full : Bool
  stopAutomaticRetry : Bool
  deriving ToJson

/-- A transaction endpoint answers with full storage only for a full store:
the transaction owner's retry of other failures follows the conflict
classifier, not this model. -/
@[conformance_family]
def fullTransaction : Family FailureInput TransactionOutput where
  inputs := failures.filter fun i => StorageFailure.isFull i.code
  model := fun i =>
    { full := StorageFailure.isFull i.code
      stopAutomaticRetry := StorageFailure.stopAutomaticRetry i.code }

structure HostErrorOutput where
  full : Bool
  deriving ToJson

@[conformance_family]
def hostError : Family FailureInput HostErrorOutput where
  inputs := failures
  model := fun i => { full := StorageFailure.isFull i.code }

end Conformance.StorageFailureContracts
