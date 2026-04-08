import Lake
open Lake DSL

package proofs where
  leanOptions := #[
    ⟨`autoImplicit, false⟩
  ]

require mathlib from git
  "https://github.com/leanprover-community/mathlib4" @ "v4.18.0"

@[default_target]
lean_lib Proofs where
  srcDir := "."
