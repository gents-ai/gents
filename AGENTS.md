# Repository instructions

Gents is a Rust agent runtime with DefraDB as its control plane. Configuration,
requests, responses, sessions, tools, triggers, and schedules are documents.
The `gents` runtime crate is the core; the CLI, desktop app, protocol, and
schemas operate or expose its vocabulary. Use DefraDB's DID identity and
document ACP for database authorization. Reuse its transaction and P2P APIs
through the existing Gents adapters.

## Foundation

For changes to legal transitions, invariants, or provider input, work in this
order:

1. Update the Lean model in `crates/gents/proofs/` and keep it free of `sorry`.
2. Update the generated or model-driven conformance tests.
3. Make the Rust implementation satisfy the contract.

Plumbing and tooling need no proof change when they preserve semantics. The
[proof map](crates/gents/proofs/README.md) identifies the modeled surfaces.

## Ownership

- Request state is only `lifecycle_state`, using
  `gents_protocol::request_lifecycle::RequestLifecycleState`. Claimed work runs
  through the owned completion loop; reuse its lifecycle and terminal owners.
- The owned loop is the sole provider-input boundary. Durable transcripts may
  be permissive; sanitize and narrow them there.
- DefraDB authenticates actors as DIDs and enforces document authorization
  through ACP. Keep Gents principals bound to those DIDs; do not add a parallel
  identity or authorization layer. Behaviors are reusable interfaces, and
  deployments place principals on hosts.
- Tool selections, MCP services, subagent targets, skills, tasks, schedules,
  and event triggers are documents. Extend their existing reconcilers and
  owners instead of adding side channels.
- Client sync has one observation owner: `gents::p2p_observability` adapts
  DefraDB status, `ClientSyncStateOwner` combines facts, and
  `project_sync_health` derives product state. Keep runtime readiness separate
  and do not add UI-local sync heuristics.
- Rig is a provider client behind `llm::rig_compat` and `provider_input`.
  Persisted messages remain native. DefraDB is the pinned public dependency in
  the workspace `Cargo.toml`; investigate node, schema, identity, and
  transaction behavior there.

## Repository rules

- Escape every interpolated GraphQL string with
  `graphql::escape_graphql_string()`.
- Never emit `[]` in a DefraDB mutation; use `null` for an empty nillable list.
- Use `tracing`, never `println!`.
- Treat flaky tests as defects: reproduce, file, and fix them.
- Create worktrees with `make worktree BRANCH=<branch>` so build artifacts are
  cloned efficiently.

Before pushing, run `cargo test -p gents` and
`cargo check --workspace --all-targets`. Run `lake build` for proof changes and
the relevant CLI or desktop suites for affected consumers.
