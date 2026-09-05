# Single Owner Milestone — PR Stack

Stacked on #1344 (`single-owner`, issue #1330). Each PR is one issue, one branch, based on the previous PR's branch, executed with the same flow as #1344: plan → Sonnet implementer per task → Sonnet task review → fix rounds → whole-branch review → push. Every branch is a clean cutover: no compat shims, no serde defaults for old shapes, net code deletion.

| # | issue | branch | base | scope |
|---|---|---|---|---|
| 1 | #1340 | `so/1340-init-summary` | `single-owner` | `DesktopInitSummary` camelCase + generated TS; live bug on main |
| 2 | #1337 | `so/1337-cli-tool-managed-exec` | previous | `CliTool` subprocesses through `managed_exec` |
| 3 | #1336 | `so/1336-request-create` | previous | one `AgentRequestCreate` constructor |
| 4 | #1335 | `so/1335-goal-tokens` | previous | `Goal.tokens_used` via `provider_usage`; `/self` context indicator |
| 5 | #1332 | `so/1332-backend-availability` | previous | one backend availability predicate; readers consume readiness |
| 6 | #1333 | `so/1333-mcp-health` | previous | MCP health projected in Rust; delete TS `stuck` |
| 7 | #1334 | `so/1334-subagent-tree` | previous | one subagent tree builder in `gents` |
| 8 | #1331 | `so/1331-config-validators` | previous | one validator per config document; persona_ops for behavior writes |
| 9 | #1338 | `so/1338-provider-owner` | previous | backend client construction, credential resolution, OAuth bearer wrapper |
| 10 | #1339 | `so/1339-small-consolidations` | previous | checklist issue |
| 11 | #1342 | `so/1342-typed-row-boundary` | previous | `Option<RequestLifecycleState>` at the row boundary |
| 12 | #1343 | `so/1343-projection-policy` | previous | one rule for request state in external projections |

#1341 (request execution lease) is Lean-heavy and stands alone after the stack.

Per-PR plans live beside this file as `2026-09-0X-<issue>-<slug>.md`.
