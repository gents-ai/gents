import Proofs.SelfConfig.Types
import Proofs.SelfConfig.Apply
import Proofs.SelfConfig.Theorems
import Proofs.SelfConfig.Cases

/-!
# Self-Configuration Model (#654)

Typed, DID-scoped agent self-configuration writes: per-target writable/
protected field partitions, patch merge semantics, and the write step with
validation and the opt-in no-lockout guard.

Proven properties:
- T-SC1 identity immutability (`identity_immutable`,
  `runStep_identity_immutable`)
- T-SC2 field containment (`containment`, `applyPatch_protected`)
- T-SC3 transactional totality (`step_accepts_wholesale`,
  `runStep_reject_frame`, `runStep_accept_frame`, `runStep_accept_target`)
- T-SC4 no-lockout recoverability (`no_lockout_recoverable`)
-/
