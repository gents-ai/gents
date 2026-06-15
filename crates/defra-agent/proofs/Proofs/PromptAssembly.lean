import Proofs.PromptAssembly.State
import Proofs.PromptAssembly.Executable
import Proofs.PromptAssembly.Template
import Proofs.PromptAssembly.Properties

/-!
# PromptAssembly (barrel) — issue #448

Provider-input narrowing and prompt-layer assembly: the durable transcript
is permissive; `sanitize` (Rust `compaction::sanitize_history_for_provider`,
applied at the `run_loop_stream` entry chokepoint) narrows it to the strict
provider format. See `Proofs/PromptAssembly/Properties.lean` for the
contract theorems (T1 soundness — which subsumes T4 split-stability, T2
fixpoint/preservation, T3 idempotence, T5 loop-threading validity, and the
assembly-order lemmas).

Rust conformance: `crates/defra-agent/tests/conformance/prompt_assembly.rs`.
-/
