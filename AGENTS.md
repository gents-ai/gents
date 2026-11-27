# Repository instructions

## You are here

Gents is an agent runtime: a small Rust core runs the owned agent loop and keeps
its state as documents in DefraDB ([defradb.rs](https://github.com/sourcenetwork/defradb.rs)),
which supplies storage, transactions, DID identity, access control (ACP) and
peer-to-peer sync. Capabilities are plugins in packs; clients are adapters.

Crates are layered: each names only crates below it
(`crate_dependencies_follow_the_layers`). Put code in the lowest crate that
owns everything it uses.

| Layer (bottom up) | Crate | Owns |
| --- | --- | --- |
| Vocabulary | `gents-schemas`, `gents-protocol`, `gents-lean-vocab`, `gents-loop`, `gents-claude` | SDL and access policy; wire types, operation contracts, typed queries; Lean case decoders; the provider turn loop and provider input; the Claude wire protocol |
| Store | `gents-store` | DefraDB access, transactions, identity, ACP |
| Documents | `gents-documents` | Configuration documents, desired state |
| Inference, P2P | `gents-inference`, `gents-p2p` | Backends and accounts; peers, enrollment, sync |
| Packs | `gents-pack` | Packs, the plugin runner |
| Session | `gents-session` | Requests, sessions, the lifecycle hook seam |
| Tools | `gents-tools` | The tool surface, the Engineer's self-config |
| Tool calls | `gents-toolcalls` | Tool-call lifecycle, background jobs |
| Automation | `gents-automation` | Triggers, tasks, callbacks, Goals |
| Observe | `gents-observe` | Timelines, trace export |
| Root | `gents` | The runtime and its composition |
| Host, clients | `gents-host`, `gents-service`, `gents-cli`, `gents-shell`, `gents-eval`, `gents-desktop*`, `gents-client-host`, `gents-media-host`, `apps/`, `packages/` | Serving, service supervision, adapters, presentation |
| Unlayered leaves | `gents-chatgpt-login`, `gents-claude-login`, `gents-login-ui`, `gents-fs-runner`, `gents-core-packs`, `gents-lean-contract`, `gents-workspace-hack` | Login flows, the filesystem runner, core pack archives, the Lean contract loader, hakari feature unification |

| Embedding leaf | `gents-memory-embed` | The bundled in-process multilingual embedder and its weights |

Lean models live in `crates/gents/proofs/Proofs/`, one directory per layer,
and import only downward; TLA+ specs live in `crates/gents/proofs/tla/`; the
[proof map](crates/gents/proofs/AGENTS.md) names the modelled surfaces. Read
versions from `Cargo.toml`, `Cargo.lock` and `rust-toolchain.toml`.

**Worktrees.** One tree per change, created from the main checkout with
`git fetch origin && make worktree BRANCH=<branch> BASE=origin/main`. Build
with `PROFILE=fast-dev` (Make) or `--profile fast-dev` (Cargo); direct test
runs need the `RUST_MIN_STACK` the Makefile exports. Never change a checkout while a build or test runs in it; a
timed-out observation is not a stopped process. Remove the worktree once its
branch merges; `make sweep` clears stale artifacts.

## Model first

The Lean owners are the truth: when model and Rust disagree, decide in the
model. For a transition, invariant, access rule or provider input:

1. **Model.** Change the Lean owner; no `sorry`. Interleavings of concurrent
   actors go in TLA+: a spec, a correct configuration, and one broken
   configuration per safeguard that fails as expected
   (`crates/gents/proofs/tla/scripts/run-tlc.sh <config>`).
2. **Conformance.** Mark the owner `@[conformance_function]` (its whole domain
   becomes cases) or `@[conformance_family]` (chosen cases), run
   `make lean-regen`, and replay it against the real production function or
   real runtime/e2e fixture (`BootedRuntime`, `SignedHome`, `ClaimedRequest`)
   in exactly one test, written in the same change as the family
   (`every_emitted_family_has_one_consumer_or_a_ledger_entry`; the ledger
   holds only boundaries and a shrinking open list, see
   [the proofs guide](crates/gents/proofs/AGENTS.md)).
   Never replay against a test stand-in or a `#[cfg(test)]` production bypass.
   Expectations come only from running the model: no fixture constants, no
   Rust mirror of its policy.
3. **Rust.** Make the owner satisfy the contract.

Lean and TLA+ divide the work: Lean owns every decision (a pure function from
observed state to action, with its theorems); TLA+ owns the interleaving of the
actors that call it. A spec names the Lean owner of each decision it uses and
takes the decision's cases from that owner rather than restating the policy;
each configuration records its expected result (holds, or fails with the
property it names), so a spec that stops catching its bug is visible.

Generated files (decoders, the family schema, the access policy YAML) are
regenerated, never edited. Decisions and external premises are docstrings on
the Lean definitions they justify. Semantics-preserving plumbing needs no model
change.

## Flaky tests are defects

An intermittent failure is an interleaving nobody wrote down. Never answer one
with a sleep, retry, larger deadline or looser assertion.

1. State the property the test asserts over named actors and steps; if it
   cannot be stated, the test asserts an accident.
2. Model the interleaving in TLA+ from the code's real atomic steps.
3. Fix the side the model says is wrong: the product if the checker finds a
   violating trace, the test or its observer if the model exonerates the
   product. The owner's decision goes in Lean with a conformance family.
4. Keep the test as the regression and show it passing under the contention
   that broke it. Report what was verified by execution, by model checking and
   only by reading.

An intermittent correctness assertion (a lost update, message or stop) is a
possible product defect until the model says otherwise.

Waits follow events: subscribe to the documents watched (`read_when_changed`,
`rows_when` in `gents_store::test_support`) or await the owner's notification
(such as a `HeldCall::entered()`), failing with the last state seen once
nothing changes for the suite-wide `STALL_CEILING`. When a fixture binds a
deadline the scenario does not exercise, use `UNREACHED_DEADLINE` or
`unreached_deadline_at()` (`gents_store::test_support`). Never add `sleep(`
calls in tests (`test_sleeps_only_shrink`, `TEST_SLEEPS` only shrinks) or
arbitrary per-test wall-clock budgets like
`tokio::time::timeout(Duration::from_secs(5), ...)`; assert the order of
events, never wall-clock latency.

## Code shape

- **No duplication.** Reuse the owner: no parallel interpretation, side
  channel, alias or re-export from an old location. Do not hand-maintain a list
  a scan can derive.
- **Modular and small.** One responsibility per module; split files and crates
  at an ownership seam instead of growing them. No size limit is enforced;
  Clippy's complexity thresholds are pinned in `clippy.toml`, fixed by
  restructuring, never raised.
- **Point dependencies down.** When a lower layer needs behavior above it,
  define a small trait where it is called and implement it above.
- **No comments** except a constraint code and proofs cannot express (safety,
  ordering, external behavior), stated once on the item.
- **No prose documents** (design notes, plans, status reports) in this
  repository; user documentation lives in the docs repository.
- **No backwards compatibility before 1.0**: no upgrade paths, conversions or
  aliases. An incompatible home is refused clearly and left untouched.

## Enforced rules

Unless noted, the rules are tests in `crates/gents-lean-vocab/tests/write_owner_structure/`;
an exception names its file, its exact count and its reason.

| Rule | Enforced by |
| --- | --- |
| DefraDB access goes through `gents-store`: writes `ConfigAccess::write`/`write_local`, multi-statement or DID-scoped work `ConfigAccess::transact_*`, reads `graphql_with_transaction_retry` or `ConfigAccess::execute`. Never the embedded node, raw HTTP GraphQL, or another retry loop or wrapper. | `production_defradb_writes_have_one_owner`, `production_defradb_node_execution_stays_in_the_store_owners` |
| A User-signed transaction that can lose its read to a concurrent write uses `transact_signed_conflict_rebuild` (or `_with_finality`); its callback re-reads its inputs and has no outside effects, and only a confirmed abort is rebuilt. | Lean `clientSignedWriteMayRetry` |
| Queries use `gents_protocol::document_query` with variables; any interpolated string goes through `graphql::escape_graphql_string()`. A mutation never emits `[]`; use `null`. | review |
| Owner interfaces return `thiserror` enums. An erased error is classified by asking the owner through `error_chain::find_map_source`, and only an owner asks for its own type, in a classifier beside that type; an owner passing an application error through wraps it in `error_chain::PassedThrough`. A store conflict is the typed `TransactionConflict`. Error text is decided, and a third-party error type asked for, only in an adapter classifier whose docstring states its `External premise:`; no other downcast or text decision is admitted. | `owner_interfaces_use_thiserror`, `downcasts_are_asked_only_by_owner_classifiers`, `error_text_is_decided_only_in_adapter_classifiers` |
| Request context (identity, signing, transaction, requester, trace) is an explicit argument, never thread- or task-local. | `ambient_request_context_is_banned` |
| One Tokio runtime per process, built at its entry point; handles come from the caller (`DropCleanup::current` in drop guards). Never block a runtime thread: `.await`, `spawn_blocking`, `drive_unsendable`. | `disallowed-methods` in `clippy.toml` |
| Maps and sets are `rapidhash`; trait bounds live in `where` clauses. | `hash_maps_use_rapidhash`, `trait_bounds_live_in_where_clauses` |
| Rig types stay behind `gents_loop::rig_compat` and `provider_input`. | `rig_types_stay_behind_their_owners` |
| OAuth credentials are picked through the resolver. | `oauth_credential_picks_go_through_the_resolver` |
| Test fixtures never reach a production build. | `test_fixtures_stay_dev_only` |
| Standard fixtures have one definition; `src/` never includes a `tests/` file; test-only implementations of production traits live only in a crate's `src/test_support` module. | `each_fixture_has_one_definition`, `src_includes_no_tests_file`, `production_traits_have_test_implementations_only_in_test_support` |
| Tests wait on events, not `sleep(` (`TEST_SLEEPS` only shrinks). | `test_sleeps_only_shrink` |
| Every Rust file under `src/` or `tests/` is compiled by a target (no dead source). | `every_rust_source_is_compiled` |
| Tests never touch the host's service manager: whatever reaches `gents-service` or spawns `gents` enables its `test-home-guard` from dev-dependencies. | `service_isolation_stays_test_only`, `tests_reaching_the_service_manager_are_isolated` |
| One integration binary per crate (`tests/integration.rs` with `#[path]` modules); separate binaries only for process-wide state, listed with the reason. | `integration_tests_share_one_binary_per_crate` |
| Log with `tracing`, never `println!`. | review |

DefraDB's execution boundaries hold: autocommit prepares remote inputs on the
caller and routes only prepared attempts; cache-only commits stay on their
native I/O queue (Regolith's `commit_nowait`). Missing transaction mechanics
are fixed in defradb.rs or Regolith, not hidden by a Gents retry.

## Identity and ownership

- **ACP is the only authorization.** Actors are DIDs: a **User** (key stays on
  the client; signs user configuration, plugin installs and agent creation), a
  **Node** (peer admission and sync; owns operational documents no Agent
  caused) and an **Agent** (runtime-managed; owns what it creates). Each
  transaction runs as exactly one. No second identity layer or Rust check that
  bypasses ACP; if ACP cannot express a rule, change the policy model.
- **One policy, in Lean** (`crates/gents/proofs/Proofs/Store/Identity/`).
  Permissions flow down `parent` (Node, Agent, session, its children); access
  is granted by writing a relationship and revoked by deleting it. Every
  document type gets its resource, parent and create/read/update/delete rule
  in the model first. ACP authorizes whole documents: a field with different
  writers is its own document.
- **Core is the loop and its state.** Capabilities ship as WASM plugins in packs
  ([gents-ai/packs](https://github.com/gents-ai/packs)); core adds only the
  seam. Hooks on the `gents-session` seam receive rows and return effects as
  data, never a transaction, and cannot prevent an interrupt, stop or terminal.
  A plugin job is an ordinary background tool call.
- **Request state is only `lifecycle_state`** (`RequestLifecycleState`).
  Claimed work runs through the owned completion loop, the sole provider-input
  boundary. `AgentSession` is the one session document; `Agent` is its only
  configuration selection. Tools, MCP services, skills, tasks, callbacks and
  schedules are documents reconciled by their owners.
- **Clients are adapters.** An operation's contract lives in
  `gents_protocol::operations`, its handler with the documents; clients never
  re-implement validation, defaults or permissions. Clients retain only their
  private journal, configured HTTP runtime connections, and the packs they
  approved. Their held User key signs short-lived native delegations; runtime
  access and revocation are ACP operations. P2P discovery and enrollment belong only to runtime peers.
- **Model-facing text** discloses progressively: a small core, help per
  resource, errors naming the next call; answer first, then next steps, then
  metadata.

## Tests and CI

- Tests hold the Rust to the models; prefer generated cases to hand-written
  unit tests. Replay Lean conformance families against real production owners
  or real e2e fixtures (`BootedRuntime`, `SignedHome`, `ClaimedRequest`),
  never against a test stand-in or `#[cfg(test)]` production bypass. Keep
  end-to-end tests through real owners, tests pinning text a person or model
  reads, and tests evidencing a premise about the database, filesystem,
  processes or a wire format. Never assert how something is done.
- End-to-end tests drive live inference and assert behavior (states, documents,
  tool calls), not wording. Never pin temperature 0.
- A Cargo filter matching nothing exits 0: read the test count.

**Standard fixtures.** Reach for the lowest canonical fixture that covers the
test instead of hand-rolling stores, homes, identities, requests, scripted
providers or daemon boots:

| Layer | Fixture | Crate / module | Provides |
| --- | --- | --- | --- |
| Store | `TestStore` | `gents_store::test_support` | Embedded DefraDB `ServedHome` on an isolated temp home/lock |
| Documents | `SignedHome` | `gents_documents::test_support` | Enrolled operator User, published `Node` desired state and actor-scoped `ConfigAccess` over `TestStore` |
| Inference | `ScriptedBackend`, `HeldCall` | `gents_inference::test_support::scripted_backend` | Deterministic scripted HTTP provider reached by the production backend client (`entered`, `release`, `fail`, `close`) for turn, stream, EOF and interrupt control |
| Session | `AgentHome`, `ClaimedRequest` (`Stage`) | `gents_session::test_support` | Provisioned default Agent (`AgentHome`) and requests driven through production writers and `claim_with_identity` to `Submitted`, `Admitted`, `Claimed` or `Executing` |
| Runtime | `AgentRuntimeHome`, `BootedRuntime` | `gents::test_support::running_runtime` | Real `boot_runtime` over a served home bound to an HTTP backend, with readiness/slot observers, `shutdown` and `crash` |

- Never define a duplicate fixture (`each_fixture_has_one_definition` in
  `write_owner_structure::fixtures`; `FIXTURE_DUPLICATES` is empty).
- Test-only implementations of production traits live only in a crate's
  `src/test_support` module
  (`production_traits_have_test_implementations_only_in_test_support`;
  `TEST_TRAIT_IMPLEMENTATIONS` is empty), never inline in `tests/` or under
  `#[cfg(test)]` in production files.
- Never pull a `tests/` file into `src/` via `#[path]` or `include!`
  (`src_includes_no_tests_file`; `TESTS_INCLUDED_INTO_SRC` is empty).

**The gate** (`scripts/ci/gate.py`, run by `.github/workflows/ci.yml`) maps a
change to its crates and their reverse dependents and selects `fmt`, `lint`
(the product build and clippy `-D warnings`), `lint-variants` (the live suites,
the Vera and WASI builds, the desktop capabilities), `contract` (no `sorry`,
Lean layers and build, generated files current, the consumer scan, the
structure rules),
the TypeScript jobs, and the test groups `foundation`, `store`, `session`,
`runtime`, `cli` and `desktop`, each crate built and tested alone.

```bash
make gate-plan                  # what this change selects
make gate                       # run it against BASE=origin/main
make gate-run JOB=test:session  # one job
```

A draft tests the crates it changes; a ready pull request and the merge queue
add their dependents; main is not run on push: changes land through the merge queue,
whose runs test them, and the nightly run of `main` runs everything; those
completed runs are the measurement for flakes and speed. Native changes pass `make gate` before
pushing: a lower crate is not verified until its consumers, clients included,
pass.

**Runners** are chosen by role from `.github/ci-runners.json` (capability labels,
never host names); a machine joins by registering with a role's labels. Pull
request jobs also require `ci-pr`; the few runners without it (`gate.py runners`)
serve only the merge queue and the nightly run. A fork pull request never runs on
a self-hosted runner; a maintainer pushes it to a branch here to test it.

## Delivering a change

Read the issue's acceptance, the owning model and its callers first. Diagnose a
failure before rerunning it. For performance, compare equivalent workloads at
the owner and record revision, scope and result. Describe the problem, the
delivered behavior, the checks actually run and the limits that remain; close
an issue only when its whole acceptance is delivered.
