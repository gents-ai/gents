import Proofs.EditMatch.Model
import Proofs.EditMatch.Properties

/-!
# EditMatch (#738, #724)

The `edit_file` matcher: deterministic relaxation ladder (exact →
trailing-whitespace → trim/indentation → unicode-normalized), ambiguity
gating, convenience-operation desugaring, one pure decision shared by
dry-run and apply, and the #724 optimistic-concurrency stale gate.
Conformance home: `tests/conformance/edit_match.rs`.
-/
