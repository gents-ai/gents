import Proofs.MCPHealth.State
import Proofs.MCPHealth.Transition

/-!
# MCP Health / Eviction

Per-service Lean state machine for the MCP connection-pool health checker.
Four-state lifecycle (`healthy → degraded → evicted → reconnecting`)
parameterized by a failure-count threshold K. K=1 matches today's Rust;
K ≥ 2 admits the bounded-flap regime. See
`docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md` for the design.
-/

-- Subsequent imports added as tasks land:
-- import Proofs.MCPHealth.Properties
-- import Proofs.MCPHealth.Coupling
-- import Proofs.MCPHealth.Executable
