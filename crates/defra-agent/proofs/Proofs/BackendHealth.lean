import Proofs.BackendHealth.State
import Proofs.BackendHealth.Transition
import Proofs.BackendHealth.Properties
import Proofs.BackendHealth.Executable

/-!
# Backend Health (#640)

Per-runtime measured-health state machine for the scheduled inference-backend
prober: K consecutive failures demote to `unhealthy` (vetoing routing), a
single success promotes back to `healthy`, and effective availability is
`intent && !blocksRouting(measured)`. See
`docs/superpowers/specs/2026-07-07-backend-probe-health-640-design.md`.
-/
