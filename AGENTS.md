# Repository instructions

Gents is a Rust agent runtime with DefraDB as its control plane. Configuration,
requests, responses, sessions, tools, triggers, and schedules are documents.
The `gents` runtime crate is the core; the CLI, desktop app, protocol, and
schemas operate or expose its vocabulary. Use DefraDB's DID identity and
document ACP for database authorization. Reuse its transaction and P2P APIs
through the existing Gents adapters.

## Configuration refactor stack

[PR #1430](https://github.com/gents-ai/gents/pull/1430) is the specification:
its description holds the decisions and deletion handoff; canonical config/protocol
structs define the target shapes. Old runtime code is evidence of existing behavior,
not permission to restore rejected abstractions.

- Keep the spec branch focused on types. It is intentionally red; per the user,
  do not run builds/tests or add compatibility code to make it compile here.
- Implement in stacked PRs: Lean [#1436](https://github.com/gents-ai/gents/pull/1436),
  then conformance [#1443](https://github.com/gents-ai/gents/pull/1443), then
  Rust/schema/loading and packs/consumers. Each PR targets its immediate parent.
  Keep its description current with baseline, owners, deletions and validation.
- Trace removed concepts through writers, readers and contracts. Preserve their
  guarantees through existing owners; delete obsolete implementations in their
  owning layer. Do not duplicate configuration types or graph selection paths.
- Remove historical migration steps, frozen old schemas and transition-specific
  tests; retain useful migration infrastructure. No legacy conversions or data wipes.
- Resolve spec conflicts in #1430 before propagating them. Validate each descendant
  and the integrated stack; the non-buildable foundation must not merge alone.

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
  identity or authorization layer. Behaviors are reusable interfaces. The
  operating convention is one active runtime per principal; enforcement is
  deferred. Do not add host identity, leases, or host-migration machinery. Validate
  configured paths/resources through existing owners and fail affected work
  clearly when unavailable. Paths/observations are not portable identity data.
- Tools, MCP services, subagent targets, skills, tasks, callbacks, schedules,
  and event sources are documents. Tool groups are nested settings within Tools.
  Extend existing reconcilers and owners instead of adding side channels.
- Task hooks are explicitly configured host commands on Task. Reuse existing
  host process execution and request completion owners for ordering, timeouts,
  errors, and cancellation; do not introduce a TaskRun lifecycle. Hook state lives
  on the host, not in input/result mapping configuration. Existing event callbacks
  retain their action journals. Workspace integration with general hooks remains
  an explicit design TODO; preserve current workspace functionality until resolved.
- AgentSession is the single durable session document; AgentConversation is retired.
  Behavior is its only configuration selection. Title and creation provenance live
  within the session; request state and derived UI observations keep their existing
  owners. Use the canonical protocol session type rather than another writable copy.
- Client sync has one observation owner: `gents::p2p_observability` adapts
  DefraDB status, `ClientSyncStateOwner` combines facts, and
  `project_sync_health` derives product state. Keep runtime readiness separate
  and do not add UI-local sync heuristics.
- Rig is a provider client behind `llm::rig_compat` and `provider_input`.
  Persisted messages remain native. DefraDB is the pinned public dependency in
  the workspace `Cargo.toml`; investigate node, schema, identity, and
  transaction behavior there. Claude subscriptions use Anthropic Messages HTTP
  with an agent-scoped `OAuthCredential` written by `gents claude-login` and
  refreshed by gents; the `claude` binary is not a dependency.

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
