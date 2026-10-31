# Repository instructions

Gents is a Rust agent runtime with DefraDB as its control plane. Configuration,
requests, responses, sessions, tools, triggers, and schedules are documents.
The `gents` runtime crate is the core; the CLI, desktop app, protocol, and
schemas operate or expose its vocabulary. Use DefraDB's DID identity and
document ACP for database authorization. Reuse its transaction and P2P APIs
through the existing Gents adapters.

## Finding the owner

Read versions and dependency revisions from `Cargo.toml`, `Cargo.lock` and
`rust-toolchain.toml`; read the actual check matrix from `.github/workflows/`.
Do not duplicate changing versions in instructions.

| Surface | Starting point |
| --- | --- |
| Runtime documents, admission, tasks and recovery | `crates/gents/src/` |
| Provider turn execution and input assembly | `crates/gents-loop/src/` |
| Canonical wire types and database vocabulary | `crates/gents-protocol/`, `crates/gents-schemas/` |
| Executable models and model-driven contracts | `crates/gents/proofs/`, `crates/gents-lean-contract/` |
| CLI and installed editor helpers | `crates/gents-cli/`, `crates/gents-client-host/`, `crates/gents-media-host/` |
| Desktop application | `apps/gents-desktop/` |
| Shared desktop client and presentation | `crates/gents-desktop-core/`, `crates/gents-desktop/`, `packages/` |

## Working on a change

- Read the issue's acceptance, the owning contract and its callers before
  editing. Reuse the existing owner rather than adding a parallel interpretation.
  Keep each module focused on one responsibility; split at ownership seams.
- Preserve unrelated edits and use an isolated worktree. Do not change a
  checkout's source, HEAD, manifests or dependency pin while its build or test is
  running. An observation timeout is not a stopped process; inspect its actual
  state before restarting it. Stop only processes owned by the current work.
- Diagnose a retained failure before rerunning it. Use the smallest regression
  that distinguishes the defect; do not hide it with retries, larger deadlines
  or looser assertions. Reuse valid evidence from unchanged inputs.
- For performance changes, compare equivalent workloads at the affected owner
  and record the source revision, scope and measured result. A narrower payload
  alone does not establish faster execution or fewer database reads.
- Keep review changes scoped to their final behavior and target the immediate
  parent when stacked. Describe the problem, delivered behavior, actual checks
  and remaining limits. Close an issue only when its complete acceptance is
  delivered; record partial work without claiming the whole flow is finished.

## Ownership

- Core is bounded to the owned agent loop and task and data management in
  DefraDB: documents, triggers, tasks, identity and ACP, plus
  generic plugin seams (tools, callbacks, plugin jobs). Workflow languages and
  other capabilities (OCR, git and GitHub, web research, domain tools) ship as WASM
  plugins in packs (gents-ai/packs) and run as tools or workflow stages; core
  adds only the plugin seam a pack needs, not the capability.
- Request state is only `lifecycle_state`, using
  `gents_protocol::request_lifecycle::RequestLifecycleState`. Claimed work runs
  through the owned completion loop; reuse its lifecycle and terminal owners.
- The owned loop is the sole provider-input boundary. Durable transcripts may
  be permissive; sanitize and narrow them there.
- DefraDB authenticates actors as DIDs and enforces document authorization
  through ACP. Keep Gents node identity bound to those DIDs; do not add a parallel
  identity or authorization layer. Agents are reusable configuration. The
  operating convention is one active runtime per node; enforcement is
  deferred. Do not add host identity, leases, or host-migration machinery. Validate
  configured paths/resources through existing owners and fail affected work
  clearly when unavailable. Paths/observations are not portable identity data.
- Tools, MCP services, agent targets, skills, tasks, callbacks, schedules,
  and event sources are documents. Tool groups are nested settings within Tools.
  Extend existing reconcilers and owners instead of adding side channels.
- Task hooks are explicitly configured host commands on Task. Reuse existing
  host process execution and request completion owners for ordering, timeouts,
  errors, and cancellation; do not introduce a TaskRun lifecycle. Hook state lives
  on the host, not in input/result mapping configuration. Existing event callbacks
  retain their action journals. A plugin job is an ordinary background tool
  call; its pin and progress are fields on that call; guest notes are pack
  data; it is not a TaskRun lifecycle or a new core action journal. Workspace integration with general hooks remains
  an explicit design TODO; preserve current workspace functionality until resolved.
- AgentSession is the single durable session document.
  Agent is its only configuration selection. Title and creation provenance live
  within the session; request state and derived UI observations keep their existing
  owners. Use the canonical protocol session type rather than another writable copy.
- Client sync has one observation owner: the p2p observability adapter reads
  DefraDB status, `ClientSyncStateOwner` combines facts, and
  `project_sync_health` derives product state. Keep runtime readiness separate
  and do not add UI-local sync heuristics.
- Rig is a provider client behind `gents_loop::rig_compat` and
  `gents_loop::provider_input`, re-exported by the inference owner
  (`llm::rig_compat`) and the turn owner (`provider_input`) for callers here. Rig's own types stay inside those two
  and the loop that drives them (`gents_loop::loop_stream`); runtime owners
  use their native handles (`ProviderModel`, `ProviderClient`). For now,
  provider transport adapters (the `*_http`, Codex, Claude Messages, xAI and
  backend-client modules) may also implement Rig's HTTP client traits or
  build its provider clients. `rig_types_stay_behind_their_owners` in
  `write_owner_structure.rs` enforces the boundary; its allowlist only shrinks.
  Persisted messages remain native. DefraDB is the pinned public dependency in
  the workspace `Cargo.toml`; investigate node, schema, identity, and
  transaction behavior there. Claude subscriptions use Anthropic Messages HTTP
  with a node-scoped `OAuthCredential` written by `gents claude-login` and
  refreshed by gents; the `claude` binary is not a dependency.

## Foundation

For changes to legal transitions, invariants, or provider input, work in this
order:

1. Update the Lean model in `crates/gents/proofs/` and keep it free of `sorry`.
2. Update the generated or model-driven conformance tests.
3. Make the Rust implementation satisfy the contract.

Plumbing and tooling need no proof change when they preserve semantics. The
[proof map](crates/gents/proofs/README.md) identifies the modeled surfaces.

Conformance has one semantic source: the executable Lean owners. Serialize actual
modeled inputs/actions and derive expectations through those owners; do not keep
separate fixture constants or test-only Rust policy machines that mirror their behavior.
Explicit regression expectations must be checked against model execution. Wire
decoders and native adapters translate representations, not redefine policy.

## Repository rules

- Escape every interpolated GraphQL string with
  `graphql::escape_graphql_string()`.
- Never emit `[]` in a DefraDB mutation; use `null` for an empty nillable list.
- All application DefraDB access goes through the store layer's config_client
  and graphql owners: single writes via `ConfigAccess::write`/`write_local`,
  multi-statement or DID-scoped work via `ConfigAccess::transact*`, reads via
  `graphql_with_transaction_retry` (embedded) or `ConfigAccess::execute`
  (local/HTTP). Never call `EmbeddedNode::execute*`, `runner()`, or raw DefraDB
  HTTP GraphQL from feature code, and never add another retry loop or access
  wrapper. `write_owner_structure.rs` is a syntactic ratchet over direct node
  access, not complete enforcement; its allowlist only shrinks.
- Owner and library interfaces, the seams where callers decide behavior by
  which failure occurred, return module-owned `thiserror` enums, and classifiers
  match those enums or ask the owner's classifier. `OWNER_MODULES` in
  `owner_interfaces_use_thiserror` lists them. Application code (commands, tool
  handlers, daemon glue, tests) may use `anyhow` with context; where an erased
  error reaches an owner boundary, find the owner enum with
  `gents_protocol::error_chain::find_map_source` and ask it, since `source()`
  skips a transparent variant's own error. A decision on error text belongs only
  in an adapter-boundary classifier whose docstring names the external premise
  (third-party or persisted text is the only signal).
  `error_text_classification_only_shrinks` and `downcast_classifiers_only_shrink`
  ratchet both; their allowlists only shrink. Print typed error chains with
  `gents_protocol::error_chain::ErrorChain`.
- Request context (identity, signing, ACP bypass, transaction handles, the
  requester or session, trace or request IDs) travels with the task or message
  as an explicit argument or field, never in `thread_local!` or `task_local!`.
  `ambient_request_context_only_shrinks` in `write_owner_structure.rs` bans new
  production sites; its allowlist only shrinks.
- Preserve DefraDB's execution boundaries when updating its adapter or dependency.
  Autocommit prepares remote inputs on the caller and routes only prepared database
  attempts; do not replace that sequence with a call that moves the whole request
  onto a database worker. Worker-affine cache-only commits stay on their native I/O
  queue through `commit_nowait`; never offload them to a pool's blocking executor
  or add a Gents retry to hide `WouldBlock::Io`. Fix missing asynchronous transaction
  mechanics in Regolith. Read the owning contracts before changing these paths:
  [PooledExecutor](https://github.com/sourcenetwork/defradb.rs/blob/main/crates/query/src/pooled/mod.rs),
  [embedded autocommit](https://github.com/sourcenetwork/defradb.rs/blob/main/crates/defra-node/src/lib.rs),
  [WorkerIo](https://github.com/sourcenetwork/defradb.rs/blob/main/crates/storage/src/backends/regolith/worker_io/mod.rs),
  and [Regolith commit tickets](https://github.com/sourcenetwork/regolith/blob/main/src/transaction/nowait.rs).
- Use `tracing`, never `println!`.
- One Tokio runtime per process, built at its entry point (`main`, the Tauri
  runtime install, `#[tokio::test]`); the speech worker's silent runtime is the
  one documented exception. Code that needs a `tokio::runtime::Handle` takes it
  from its caller; `tokio::spawn` inside a running task is fine. Never call
  `Handle::current`/`try_current`: a guard whose `Drop` owes async cleanup
  binds its runtime with the drop-cleanup owner's `DropCleanup::current`, and tests
  take theirs from their crate's test-support helper.
- Never block a runtime thread: every wait is an `.await` (`tokio::time::sleep`,
  the owner's notification, never a polling loop) and synchronous work runs on
  `spawn_blocking`. Clippy bans `std::thread::sleep`, `Handle::block_on`,
  `block_in_place` and the futures and tauri `block_on`; each remaining use
  carries a reasoned `expect` and sits off the runtime workers (a synchronous
  process or test, a dedicated OS thread, a `spawn_blocking` closure) or is a
  test future that deliberately overruns a deadline within one poll.
  `Runtime::block_on` is for process and test entry points and dedicated
  threads. Drive a non-`Send` embedded-node future through the
  store layer's storage backend (`drive_unsendable`).
- Every hash map and set is `rapidhash::RapidHashMap`/`RapidHashSet` (the fast
  hasher, built with `new`, `default` or `with_capacity` via `HashMapExt`/
  `HashSetExt`), and hashers are `rapidhash::fast::RapidHasher`, as in DefraDB.
  Use std's map only where a third-party signature fixes it;
  `hash_maps_use_rapidhash` ratchets this and its allowlist only shrinks.
- Trait bounds live in where clauses, never inline in generic parameter lists;
  `trait_bounds_live_in_where_clauses` ratchets this.
- Treat flaky tests as defects: reproduce, file, and fix them.
- Create worktrees with `make worktree BRANCH=<branch>` so build artifacts are
  cloned efficiently. In agent worktrees build and test with
  `PROFILE=fast-dev`; the default dev profile keeps full dependency debug info
  and fills the disk across concurrent worktrees. Run `make sweep` to delete
  stale artifacts, and remove a worktree (`git worktree remove`) once its
  branch merges.

## Documentation and comments

Sources of truth are the Lean model (`crates/gents/proofs`), the code, and the
tests that bind them. Do not add prose documentation, design notes, status
reports or working documents to this repository; derive them with agents when
needed. User documentation belongs in the docs repository.

Record decisions, external premises and safety reasons that cannot be derived
from code or proofs as docstrings on the Lean definitions and theorems they
justify. For constraints on native interfaces the model does not cover, use a
docstring on the owning Rust or TypeScript item instead. Other code comments
state only constraints the code and proofs cannot express (safety, ordering,
external-system behavior); do not narrate code, restate names or record history.

Keep agent instructions (this file), CHANGELOG, license, generated references,
machine-read manifests and maps, runtime-loaded prompt and pack markdown, and
minimal install steps. Before deleting prose, move any non-derivable obligation
it holds to its owner, and never remove a file a build, test or coverage check
reads.

Model-facing tool text and agent prompts use progressive disclosure (#2088): a
small tool core, then help that adds detail per resource and command, and
errors that name the next call. Prompts hold concepts, tools hold syntax and
recipes; nothing is repeated across layers or on every response. Model-facing
tool results read top to bottom: the answer first, then how to proceed, then
metadata.

## Specification stacks

### Configuration refactor stack

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
  owning layer. Do not duplicate configuration types or selection paths.
- Remove historical migration steps, frozen old schemas and transition-specific
  tests; retain useful migration infrastructure. No legacy conversions or data wipes.
- Resolve spec conflicts in #1430 before propagating them. Validate each descendant
  and the integrated stack; the non-buildable foundation must not merge alone.

### Canonical output stack (#1571)

`gents_protocol::output` and the SDL are the breaking specification; the
[ownership table](contracts/canonical-output.md) assigns the remaining deletions.
Keep this layer intentionally red: no builds/tests or compatibility shims.
Stack Lean, generated conformance, then implementation/consumers, each on its
parent. Model the new contracts before implementing transitions or projections.
The foundation must not merge alone; validate the integrated stack before pushing
its implementation layers.

### Session-message stack (#1938)

The session-message spec layer (`gents_protocol::request_admission`, the
AgentRequest/AgentToolCall/Node SDL, `AgentTools` and
`Proofs/Request/CausalHop.lean`) removes the privileged subagent position.
Keep it intentionally red: no Rust builds, tests or compatibility shims. Stack
L1 spec, L2 conformance, L3 runtime, L4 CLI/shims, L5 desktop, each on its
parent. The spec layer never merges alone; validate the integrated stack
before pushing its implementation layers.

### Node/Agent rename stack (#1799)

The rename spec layer (Node and Agent SDL, `gents_protocol` re-exports, the
Engineer prompt and preset) replaces the old vocabulary in the table below.
Keep it intentionally red: no Rust builds, tests or compatibility shims. Stack
L0 spec, L1 Lean, L2 conformance, L3 runtime, L4 CLI/shims, L5 desktop, each
on its parent. The spec layer never merges alone; validate the integrated stack
before pushing its implementation layers.

| Old | New |
| --- | --- |
| AgentPrincipal | Node |
| AgentBehavior | Agent |
| SubagentTools, SubagentTarget | AgentTools, AgentTarget |
| setup steward | Engineer |

## Validation

During iteration, run focused checks for the changed owner or flow. Do not
repeat broad suites, dependency installation or native/shared builds on every
edit. Recheck when the source, dependency inputs or a concrete failure warrants
it. Keep test, browser and installed-editor evidence distinct.

Before pushing native implementation changes, run `cargo test -p gents`,
`cargo check --workspace --all-targets` and `make lint` (clippy, `-D warnings`).
Run `lake build` for proof changes and the relevant CLI or desktop suites for
affected consumers. The named intentionally red specification layers above
retain their own validation and integration constraints.

In worktrees, select `PROFILE=fast-dev` for Make recipes or `--profile fast-dev`
for direct Cargo build/test commands. Read recipe and feature names from the
Makefile and owning manifests rather than inventing an equivalent command.
Instruction-only edits need content, reference and discovery checks; workflow-only
edits need their affected workflow/control checks. Neither alone requires a
runtime rebuild or the native implementation gates.
