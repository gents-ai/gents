import Proofs.MCPHealth.State
import Proofs.ToolExecution.Policy

namespace Proofs.MCPHealth

def healthProjection : HealthState → ToolExecution.Health
  | .healthy      => .healthy
  | .degraded     => .stale
  | .evicted      => .unreachable
  | .reconnecting => .unreachable

namespace Coupling

theorem c1_evicted_blocks_dispatch (schema : ToolExecution.SchemaStatus) :
    ToolExecution.preflight (healthProjection .evicted) schema
      = .block .serviceUnavailable := by
  cases schema <;> rfl

theorem c2_reconnecting_blocks_dispatch (schema : ToolExecution.SchemaStatus) :
    ToolExecution.preflight (healthProjection .reconnecting) schema
      = .block .serviceUnavailable := by
  cases schema <;> rfl

theorem c3_healthy_dispatches
    (schema : ToolExecution.SchemaStatus) (hv : schema ≠ .invalid) :
    ToolExecution.preflight (healthProjection .healthy) schema = .dispatch := by
  cases schema
  · rfl
  · rfl
  · exact absurd rfl hv

theorem c4_degraded_dispatches
    (schema : ToolExecution.SchemaStatus) (hv : schema ≠ .invalid) :
    ToolExecution.preflight (healthProjection .degraded) schema = .dispatch := by
  cases schema
  · rfl
  · rfl
  · exact absurd rfl hv

end Coupling
end Proofs.MCPHealth
