# Repository instructions

## What Gents is

Gents is a modular agent framework. A small core runs the agent loop and keeps
its state; almost everything else is a plugin.

- **State is documents.** Configuration, agents, sessions, requests, tool
  calls, tasks and triggers are documents in DefraDB, embedded through
  [defradb.rs](https://github.com/sourcenetwork/defradb.rs). DefraDB supplies
  the distributed-systems pieces: storage, transactions, DID identity, document
  access control (ACP) and peer-to-peer synchronization. Gents does not rebuild
  any of them.
- **Capabilities are plugins.** Tools, workflow languages and domain features
  ship as WASM plugins in packs ([gents-ai/packs](https://github.com/gents-ai/packs)).
  Features that need the request lifecycle (Goals, Mailbox) are bundled plugins
  on the lifecycle hook seam. Core adds the seam a plugin needs, never the
  capability.
- **The Engineer is how people approach it.** The Engineer is the agent a new
  node starts with. It configures the node on the user's behalf through the
  same operations every client uses, and it administers its Node: if a user
  asks it to do something, it can.
- **Clients are adapters.** The CLI, the desktop app, the editor host, the
  Engineer's tools and the runtime's HTTP surface all call the same handlers.

## How a change is made

Behavior is specified in Lean, enforced by tests the models drive, and
implemented in Rust. Work in that order:

1. **Model.** Change the Lean model under `crates/gents/proofs/Proofs` and keep
   it free of `sorry`. The model is the truth: when model and Rust disagree,
   decide which is right in the model first.
2. **Conformance.** The model emits the cases; a conformance test replays them
   against the production function that owns the behavior. Expectations come
   only from executing the model. Do not write fixture constants or a Rust
   re-implementation of a model's policy.
3. **Rust.** Make the implementation satisfy the contract.

Plumbing that preserves semantics needs no model change. Read versions from
`Cargo.toml`, `Cargo.lock` and `rust-toolchain.toml`, and the check matrix from
`.github/workflows/`; do not restate them here.

## Structure

The runtime is layered crates. A crate depends only on crates below it, and
Cargo enforces that.

| Layer | Crate | Owns |
| --- | --- | --- |
| Vocabulary | `gents-protocol`, `gents-loop`, `gents-schemas` | Wire types, operation contracts, typed queries; the provider turn loop; the SDL, the access policy and generated field constants |
| Store | `gents-store` | Database access, transactions, identity, ACP |
| Documents | `gents-documents` | Configuration documents and desired state |
| Inference, P2P | `gents-inference`, `gents-p2p` | Backends, profiles, accounts; peers, enrollment, sync |
| Packs | `gents-pack` | Packs and plugin wiring |
| Session | `gents-session` | Requests, sessions, workspace, the lifecycle hook seam |
| Tools | `gents-tools` | The tool surface, built-in tools, the Engineer's self-config |
| Tool calls | `gents-toolcalls` | Tool-call execution, background jobs, the plugin executor |
| Automation | `gents-automation` | Triggers and tasks (core); Goals (bundled plugin) |
| Observe | `gents-observe` | Traces and timelines |
| Root | `gents` | The runtime, the daemon, operation entry points, composition |
| Clients | `gents-cli`, `gents-shell`, `gents-desktop-*`, `gents-client-host`, `apps/gents-desktop`, `packages/` | Adapters and presentation |

Lean models live under `Proofs/<Layer>/` in the same order and import only
downward. `Proofs/Conformance` holds what turns models into cases;
`gents-lean-vocab` decodes them for Rust tests. Evals live in `gents-eval`.

## Identity and authorization

Authorization is DefraDB's ACP and nothing else. Understand this model before
touching anything that reads or writes a document.

**Who acts**
- A **User** is a DID whose key stays on the client. Users sign user
  configuration, plugin installs and agent creation.
- A **Node** has its own key. It authenticates peer admission and
  synchronization and owns node-local operational documents that no Agent
  caused.
  Peer admission grants no document access. Approving a client's enrollment
  does: it grants that User Node administration (`approve_enrollment`).
- An **Agent** has a DID managed by the runtime that hosts it, and owns the
  data it creates. Clients connect to that hosting runtime. One active runtime
  per node is the operating convention.

Every transaction runs as exactly one of these identities. Request context
carries the actor explicitly; nothing acts on someone's behalf by borrowing
another identity.

**How access is decided**
- One access policy (the Household policy) covers every collection. It is
  written as a Lean model in `Proofs/Store/Identity`, and the checked-in
  `crates/gents-schemas/schemas/agent/agent_ownership_policy.yaml` is generated
  from that model. Change the model, regenerate the file, never edit the YAML.
- Each collection binds to a policy **resource**. A resource has **relations**
  (who stands in what relationship to a document: owner, `administrator`,
  `reader`, `writer`, `working_agent`, a `parent` document) and
  **permissions** (read, update, delete and a few named ones) defined as
  expressions over those relations.
- Permissions flow down a hierarchy through `parent`: a Node is the parent of
  its Agents and configuration, an Agent of its sessions, a session of its
  requests, tool calls and other children. `parent->read` on a child means
  "whoever can read the parent".
- Access is granted by writing a relationship, and revoked by deleting it.
  There is no other switch.

**What follows from that**
- No second identity or authorization layer, and no trusted write path that
  checks a rule in Rust and then bypasses ACP. If ACP cannot express a rule,
  change the policy model.
- A permission category shown to a user or a model is a named grouping of ACP
  grants, checked by ACP.
- Agent operations (create, clone, edit, disable, choose the default) have one
  admission for every actor and client, decided from grants alone. No Agent is
  protected by name or role. Two lockout rules hold for everyone: the default
  Agent stays enabled, and an actor cannot disable itself.
- The Engineer holds `administrator` on its Node. `agent_manager` is the
  narrow relation for an actor who should manage the Agent catalog and nothing
  else.
- Every new document type gets a deliberate place in this hierarchy, modelled
  first: its resource, its parent, who may create, read, update and delete it,
  and why it cannot widen what an Agent or a plugin can see.
- DefraDB authorizes a document update as a whole, not per field. A field that
  needs different writers from the rest of its document belongs in its own
  document.

## Rules for code

**Layering**
- Put code in the lowest crate that owns everything it uses. Plain vocabulary
  sinks; a caller that needs a higher layer moves up.
- When a lower layer must reach behavior above it, define a small trait where
  it is called, implement it above, and hand it in or register it at the root.
- No aliases and no re-exports from an old location. A move repoints every
  user.
- Keep crates and files small. Split at an ownership seam instead of growing.
- Reuse the existing owner. Do not add a parallel interpretation, a side
  channel, a legacy conversion or a compatibility shim.

**Plugins and the lifecycle**
- Request state is only `lifecycle_state`
  (`gents_protocol::request_lifecycle::RequestLifecycleState`). Claimed work
  runs through the owned completion loop, which is the sole provider-input
  boundary.
- Lifecycle points belong to the hook seam in `gents-session`. A hook declares
  what it reads, receives rows and returns effects as data. No transaction
  handle crosses the interface, and a hook narrows authority, never widens it.
- A plugin may refuse work from starting or continuing and may request a stop.
  It can never prevent an interrupt, a stop or a terminal.

**Clients**
- An operation's contract (typed input, receipt, errors that name the next
  call) lives in `gents_protocol::operations`; its handler lives in the crate
  that owns the documents. A client does not re-implement validation, defaults
  or permission decisions.
- An edit is a patch. A busy session queues by default and steers on request.
- Clients may keep their own verbs and rendering.
- Text a model reads uses progressive disclosure: a small core, help that adds
  detail per resource, and errors that name the next call. Results read answer
  first, then how to proceed, then metadata.

**Database access**
- Build queries with `gents_protocol::document_query`; values travel as
  variables. Where a query is still a string, escape every interpolated value
  with `graphql::escape_graphql_string()`.
- Never emit `[]` in a mutation; use `null` for an empty nillable list.
- All access goes through the store's owners (`ConfigAccess`, its
  transactions, the typed read and write entry points). Do not call the
  embedded node directly and do not add a retry loop or an access wrapper.
- Preserve DefraDB's execution boundaries: autocommit prepares remote inputs on
  the caller and routes only prepared attempts; worker-affine cache-only
  commits stay on their native I/O queue through `commit_nowait`. Fix missing
  asynchronous mechanics upstream, not with a Gents retry.

**Rust**
- Owner interfaces return module-owned `thiserror` enums, and callers match on
  them. Do not decide behavior from error text. Print chains with
  `gents_protocol::error_chain::ErrorChain`.
- One Tokio runtime per process, built at its entry point. Never block a
  runtime thread: waits are `.await`, synchronous work goes to
  `spawn_blocking`.
- Request context is an explicit argument, never thread-local or task-local.
- Maps and sets are `rapidhash`. Trait bounds live in `where` clauses. Log with
  `tracing`, never `println!`.
- Provider client types stay behind `gents_loop`'s owners.

## Tests

Tests exist to hold the Rust to the models.

- **Do not write unit tests.** A behavior worth testing is worth modelling:
  add it to the model and let the generated cases drive the production code.
  When a hand-written test covers a situation the model lacks, deepen the
  model and delete the test.
- What else belongs: end-to-end tests through real owners; tests that pin text
  a model or person reads; tests that are the evidence for a premise the
  models assume about the database, filesystem, processes or a wire format.
- Never assert how something is done (query text, call counts, internal
  order, a fake's interactions).
- A test lives in the lowest crate that owns everything it uses. Shared
  fixtures sit in that crate's `test_support`, behind `test-fixtures` when
  crates above need them.
- Coverage, decoders and registries are generated or checked by a scan. Do not
  hand-maintain a list a scan can derive.
- A flaky test is a defect: reproduce it, file it, fix it. Never retry, extend
  a deadline or loosen an assertion to hide one.

## Comments and documents

- Do not write comments. Code says what it does; the models say why.
- The one exception is a constraint that neither code nor proofs can express
  (safety, ordering, the behavior of an external system), stated once on the
  item it constrains. Decisions and external premises go in docstrings on the
  Lean definitions and theorems they justify.
- Do not add prose documentation, design notes, plans or status reports to
  this repository. User documentation lives in the docs repository.

## Working in the repository

- Read the issue's acceptance, the owning model and its callers before
  editing.
- Use an isolated worktree (`make worktree BRANCH=<branch>`), build with
  `PROFILE=fast-dev` or `--profile fast-dev`, and remove the worktree when its
  branch merges. Do not change a checkout while its build or test is running.
- Iterate on the crate you changed: `cargo check -p <crate>`,
  `cargo test -p <crate>`. The crates are small so that this loop is fast.
- Before merging: `cargo fmt --all --check`,
  `cargo check --workspace --all-targets`, `make lint`, `lake build` for model
  changes, and the tests of every crate from the one you changed upward,
  including the clients. A lower crate is not verified until its consumers
  pass.
- Diagnose a failure before rerunning it. Report what was actually checked and
  what was not.
- Describe a change by the problem, the delivered behavior, the checks that
  ran and the limits that remain. Close an issue only when its whole
  acceptance is delivered.
