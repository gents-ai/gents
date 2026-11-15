import Lake
open Lake DSL

/-! `Main.lean` reads scenarios as JSON on stdin and prints the model's
decisions and stores (`lake build && lake env lean --run Main.lean`);
`scripts/ci/test_test_cache.py` compares the gate's implementation to it. -/

package testCache where
  leanOptions := #[⟨`autoImplicit, false⟩]

@[default_target]
lean_lib TestCache
