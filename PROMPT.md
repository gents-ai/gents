# Observability Rescope (#338)

**Branch:** `feat/issue-338-observability` (off `origin/main` @ df67ae63)
**Issue:** sourcenetwork/defra-agent#338
**Type:** Rescope-then-build. #338 was written as "7 agent-inside features"; two investigations (2026-06-02) show most already exist or have no consumer. **First task is to rescope the issue, then build the one feature with a real pull.**

## Why rescope — the evidence

### What already exists in defra-agent (existing-surface audit)
- **`/status`** (`crates/defra-agent-cli/src/http/router.rs:99-150`) already returns `agent_name`, `agent_did`, `version`, `uptime_seconds`, `started_at`, the agent's own `AgentRuntime` row (incl. `process_state`), all runtimes/backends, liveness, p2p. → **most of `/self` (#1) already.**
- **`/fleet/slots`** (`http/fleet_slots.rs`) + **`/metrics`** (`http/prometheus.rs:333-377`) already load per-`agent_did` state + active/pending request counts. → **#3 `/fleet` is a reshape of data already queried, not new data.**
- **Context-budget inputs all exist & persist:** `CompactionResult{original_token_estimate, compacted_token_estimate, compaction_count}` (`crates/defra-agent/src/compaction.rs:52-62`), persisted to `CompactionEntry` (`schemas/agent/compaction_entry.graphql`, written `session/compaction_entries.rs:91`), bounded by `InferenceProfile.context_window` (`schemas/inference/inference_profile.graphql:4-5`). → **#6 is surfacing/aggregation onto `/status`, not new computation.**
- **MCP introspection already exists for agents:** `discover_tools` / `describe_tool` / `call_tool` (`crates/defra-agent/src/meta_tools.rs:20,40-44`). → **#5 `mcp_list` tool is REDUNDANT; drop it.**
- **`AgentSession`** schema + machinery exist (`schemas/agent/agent_session.graphql`, `src/session/`). → **#7 `sessions` is a thin read, near-free once `defra_query` lands.**
- **Genuinely net-new:** only **#2 `defra_query`** and **#4 `memory`** (no `AgentMemory` collection, no query tool).

### What the actual consumer (Amygdala) pulls for
Amygdala has **no code dependency** on defra-agent (the dep was deliberately removed; `Cargo.lock` has zero entries). It consumes defra-agent **only as a DefraDB collection peer** via hand-rolled GraphQL (`amygdala-evals/src/defra/client.rs`), and uses **none** of defra-agent's HTTP control/observability endpoints — it rolls its own observability (`observability-mcp` over Loki/VictoriaMetrics + repo `identity.json`; defra-agent actually *calls into* that MCP, not the reverse).

What Amygdala would actually benefit from — the real pull signal:
1. **A read-only `defra_query`-style tool/API** — retires its hand-rolled GraphQL client, duplicated `escape_graphql` (`client.rs:6-19`), reimplemented terminal-state machine (`watch.rs:69-78`), polling loops (`watch.rs:80-135`, `submit.rs:339-491`), and especially the **brittle `AgentMessage.content` JSON-parse trace reconstruction** (`hydrate.rs:303-385`).
2. **A shared schema crate** — Amygdala hand-vendors `.graphql` copies (`amygdala-schemas/`), a drift hazard with no single source of truth.
3. **First-class `sampling`/`metadata` fields on `AgentRequest`** — its two "pending upstream plumbing" caveats (`submit.rs:29-30`).

**Crucially: the Amygdala pull is toward `defra_query` (the structured read/trace surface) — NOT toward `/self`, `/fleet`, `/mcp/pool`, or `memory`.** Those were authored from a speculative "agent introspecting itself" view with no current consumer.

## Scope decision (what this branch does)

### Task 0 — Rescope #338 (do first)
Post the rescope to #338: collapse "7 features" into the tiers below; note what already exists; drop `mcp_list`; park `memory`; record the two spin-out issues. (The orchestrator may have already done this — check the issue before duplicating.)

### Tier 1 — BUILD: `defra_query` read-only structured query tool
The one feature with a real consumer pull. Design it to serve **both** in-agent self-inspection **and** external trace consumers (so it can retire Amygdala's hand-rolled stack):
- Input: `{collection, filter, fields, limit}` → GraphQL → structured results.
- New tool under `crates/defra-agent/src/toolset/`, registered via `crates/defra-agent/src/tool_surface/`.
- **MUST** use `graphql::escape_graphql_string()` (CLAUDE.md convention — do not hand-roll escaping).
- Read-only; respect the agent's DID/ACP read scope (do not bypass).
- Make the result shape good enough that trace hydration (`AgentRequest`/`Response`/`Message`/`ToolCall`) no longer requires parsing `AgentMessage.content` JSON by hand.
- **Subsumes #7 `sessions`** — listing sessions becomes a `defra_query` against `AgentSession` (+ a documented recipe rather than a bespoke tool).

### Tier 2 — CHEAP SURFACING (opportunistic; small, no external pull — land if time/asked)
- **#1 `/self`**: join `AgentBehavior` (`model_name`, `backend_id`) + `InferenceBackend` provider/endpoint onto the existing `/status` payload (or a thin `/self` alias). Small.
- **#6 context budget**: aggregate `CompactionEntry` (count + latest `created_at`) + `InferenceProfile.context_window` + a live token estimate onto `/status`. Small-medium.
- **#3 `/fleet`**: per-`agent_did` reshape of the data already loaded in `prometheus.rs`/`fleet_slots.rs` (state + active/pending + `last_seen`=`AgentRuntime.updated_at`). Small.

### Dropped / parked
- **`mcp_list` tool — DROP** (redundant with `discover_tools`/`describe_tool`). If an *operator*-facing need appears, a `GET /mcp/pool` + a snapshot accessor on `McpPool` (`crates/defra-agent/src/mcp_pool.rs` has none today) is the only real gap — low priority, no current consumer.
- **`memory` tool (#4) — PARK.** No consumer pull. It's really derived-agent-memory and belongs in the #17 multi-strategy context-management design, not here. Route it there.

### Spin-out issues (adjacent, real, Amygdala-driven — file separately, not in this branch)
- **Shared agent-schema crate** (single source of truth vs. Amygdala's vendored copies).
- **`sampling` + `metadata` fields on `AgentRequest`** (the two pending-upstream-plumbing gaps).

## Build & verify
```bash
cargo test -p defra-agent
cargo test -p defra-agent-cli
cargo clippy --all-targets && cargo fmt --all
# defra_query: add a tool test that runs a filtered query against a seeded collection
```
If `defra_query` touches no state-machine behavior (it's a read tool), no Lean change is needed — but say so explicitly in the PR per CLAUDE.md's dev-flow gate.

## Provenance
Scope derived 2026-06-02 from two parallel investigations: an existing-surface audit of defra-agent and a consumer analysis of Amygdala (`jackzampolin/amygdala`), which confirmed thin, document-mediated coupling and located the real pull at `defra_query`.
