import Lake
open Lake DSL

package proofs where
  leanOptions := #[
    ⟨`autoImplicit, false⟩
  ]

require mathlib from git
  "https://github.com/leanprover-community/mathlib4" @ "v4.18.0"

-- Standalone fixture programs have separate `main`s and cannot share a barrel.
def proofRoots : Array Lean.Name := #[
  `Proofs,
  `Proofs.Conformance.ClientObservationOrdering,
  `Proofs.Conformance.ClientPresentationAgreement,
  `Proofs.Conformance.StorageWriteGate]

-- Always check the source inventory, even when compiled artifacts are cached.
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
