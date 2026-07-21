import Proofs.MCPHealth.State
import Proofs.ToolExecution.Policy

/-!
# MCP Health — Coupling to ToolExecution.Policy

Projects the four-state `HealthState` down to the three-value
`ToolExecution.Health` ADT that `preflight` already keys on. Then proves
the four bridging lemmas (C1–C4):

* Evicted / Reconnecting block dispatch as ServiceUnavailable.
* Healthy / Degraded dispatch (when schema is valid or unchecked).

This file is the **only** file in `Proofs.MCPHealth` that imports
`Proofs.ToolExecution.Policy`. It does not modify `Policy.lean` — purely
additive extension. See spec §5 for the rationale (preflight is the
correct coupling axis, not `retryDisposition`).
-/

namespace Proofs.MCPHealth

/-- Project the four-state lifecycle to the three-value Health ADT.

    Both flavors of `.degraded` (staleness-degraded and
    failure-count-degraded) project to `.stale`, reflecting that both admit
    dispatch with a longer timeout. -/
def healthProjection : HealthState → ToolExecution.Health
  | .healthy      => .healthy
  | .degraded     => .stale
  | .evicted      => .unreachable
  | .reconnecting => .unreachable

namespace Coupling

/-- C1: Evicted services block dispatch as ServiceUnavailable. -/
theorem c1_evicted_blocks_dispatch (schema : ToolExecution.SchemaStatus) :
    ToolExecution.preflight (healthProjection .evicted) schema
      = .block .serviceUnavailable := by
  cases schema <;> rfl

/-- C2: Reconnecting services block dispatch as ServiceUnavailable. -/
theorem c2_reconnecting_blocks_dispatch (schema : ToolExecution.SchemaStatus) :
    ToolExecution.preflight (healthProjection .reconnecting) schema
      = .block .serviceUnavailable := by
  cases schema <;> rfl

/-- C3: Healthy services with valid (or unchecked) schema dispatch. -/
theorem c3_healthy_dispatches
    (schema : ToolExecution.SchemaStatus) (hv : schema ≠ .invalid) :
    ToolExecution.preflight (healthProjection .healthy) schema = .dispatch := by
  cases schema
  · rfl
  · rfl
  · exact absurd rfl hv

/-- C4: Degraded services dispatch (matches today's "stale services allowed
    through with a longer timeout" behavior). -/
theorem c4_degraded_dispatches
    (schema : ToolExecution.SchemaStatus) (hv : schema ≠ .invalid) :
    ToolExecution.preflight (healthProjection .degraded) schema = .dispatch := by
  cases schema
  · rfl
  · rfl
  · exact absurd rfl hv

end Coupling
end Proofs.MCPHealth
