import Proofs.Runtime.RuntimeReconcile.SurfacePublication
import Proofs.Conformance.Family
import Proofs.Conformance.PluginStoreHome

namespace Conformance.SurfacePublication

open Lean RuntimeReconcile.SurfacePublication

deriving instance ToJson for Disposition, StartupCause, StoreWatch

attribute [conformance_function] startupCause sendProposal recreateSlot retryAfterFailure watchStore

structure EpochInput where
  opened : Option Nat
  now : Nat
  budget : Nat
  deriving ToJson

structure ReconcileInput where
  busy : Bool
  now : Nat
  deadline : Nat
  deriving ToJson

@[conformance_family]
def epochDeadline : Family EpochInput Nat where
  inputs :=
    [ { opened := none, now := 0, budget := 60000 }
    , { opened := none, now := 500, budget := 60000 }
    , { opened := some 60000, now := 59999, budget := 60000 }
    , { opened := some 60000, now := 90000, budget := 60000 } ]
  model := fun i => RuntimeReconcile.SurfacePublication.epochDeadline i.opened i.now i.budget

@[conformance_family]
def reconcile : Family ReconcileInput Disposition where
  inputs := [false, true].flatMap fun busy =>
    [(0, 60000), (59999, 60000), (60000, 60000), (90000, 60000)].map fun (now, deadline) =>
      { busy, now, deadline }
  model := fun i => RuntimeReconcile.SurfacePublication.reconcile i.busy i.now i.deadline

structure StartupInput where
  runnable : Nat
  causes : List StartupCause
  deriving ToJson

@[conformance_family]
def startupProceeds : Family StartupInput Bool where
  inputs :=
    [ { runnable := 1, causes := [.invalid] }
    , { runnable := 0, causes := [] }
    , { runnable := 0, causes := [.degraded] }
    , { runnable := 0, causes := [.publicationBusy] }
    , { runnable := 0, causes := [.publicationBusy, .degraded] }
    , { runnable := 0, causes := [.publicationBusy, .invalid] } ]
  model := fun i => RuntimeReconcile.SurfacePublication.startupProceeds i.runnable i.causes

end Conformance.SurfacePublication
