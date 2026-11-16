import Proofs.Store.NativeCompletion
import Proofs.Conformance.Family

namespace Conformance.NativeCompletionTrace

open Lean _root_.NativeCompletion

deriving instance ToJson for Caller, Event, Observation

/-- The embedded transaction path a trace runs through: a caller dropped after
native begin returned and before any statement, or a signed idempotent
transaction whose statement outlives the caller's storage step deadline. -/
inductive Path where
  | cancelledAfterBegin
  | abandonedStatement
  deriving ToJson

/-- Each step is what production does between two points a caller can
observe; its events run in order. -/
structure Input where
  path : Path
  steps : List (List Event)
  deriving ToJson

@[conformance_family]
def abandonment : Family Input (List Observation) where
  inputs :=
    [ { path := .cancelledAfterBegin
        steps := [[.callerCancelled, .registryAbandoned, .gateReleaseRequested]] }
    , { path := .abandonedStatement
        steps :=
          [ [.nativeStarted, .callerTimedOut, .registryAbandoned, .retryRequested,
              .gateReleaseRequested]
          , [.nativeReturned, .gateReleaseRequested] ] } ]
  model := fun i => observeSteps transaction i.steps

end Conformance.NativeCompletionTrace
