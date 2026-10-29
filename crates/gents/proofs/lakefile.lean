import Lake
open Lake DSL

package proofs where
  leanOptions := #[
    ⟨`autoImplicit, false⟩
  ]

require mathlib from git
  "https://github.com/leanprover-community/mathlib4" @ "v4.18.0"


def proofRoots : Array Lean.Name := #[
  `Proofs,
  `Proofs.Conformance.HttpAccess,
  `Proofs.Conformance.ClientObservationOrdering,
  `Proofs.Conformance.EnrollmentActivation,
  `Proofs.Conformance.DocumentPackUx,
  `Proofs.Conformance.ClientSubmissionReceipt,
  `Proofs.Conformance.GoalClientDelivery,
  `Proofs.Conformance.ClientVoice,
  `Proofs.Conformance.EditorUndo,
  `Proofs.Conformance.EditorUndoCheckpoint,
  `Proofs.Conformance.ClientDataPublication,
  `Proofs.Conformance.ClientRequestCacheIdentity,
  `Proofs.Conformance.ClientRequestClosure,
  `Proofs.Conformance.SessionQueueOrder,
  `Proofs.Conformance.ClientPresentationAgreement,
  `Proofs.Conformance.ClientInterruptControl,
  `Proofs.Conformance.DesktopShellSecurity,
  `Proofs.Conformance.StorageWriteGate,
  `Proofs.Conformance.StorageWriteGateAdmission,
  `Proofs.Conformance.NativeCompletion,
  `Proofs.Conformance.NativeClaimLifetime,
  `Proofs.Conformance.EvalRequestOutcome,
  `Proofs.Conformance.ToolArgumentFailure,
  `Proofs.Conformance.ParallelToolBatch,
  `Proofs.Conformance.ResponsesTermination,
  `Proofs.Conformance.StorageWriteGateStreamWaiter]

@[default_target]
target importClosure pkg : Unit := Job.async do
  proc { cmd := "python3", args := #[
    (pkg.dir / "../../../.github/scripts/check-lean-import-closure.py").toString,
    pkg.dir.toString] ++ proofRoots.map toString }

@[default_target]
target canonicalOutputMap pkg : Unit := Job.async do
  proc { cmd := "python3", args := #[
    (pkg.dir / "../../../.github/scripts/check-canonical-output-map.py").toString] }

@[default_target]
lean_lib Proofs where
  srcDir := "."
  roots := proofRoots
