# Transactional `defra-agent-cli config apply` (Issue #56)

**Date:** 2026-05-15
**Status:** Draft — brainstormed and approved by controller; awaiting written review
**Scope:** Make `defra-agent-cli config apply` atomic: either every manifest write commits or none do, with no operator intervention required after a mid-apply failure.
**Tracker:** #56 (parent #53; out-of-scope sibling #57). Lean precondition: #220/#223 (production write-boundary conformance test).

## Summary

Today `apply_desired_state_changes` in `crates/defra-agent-cli/src/config_import.rs` is best-effort: it iterates `CONFIG_APPLY_ORDER` (nine collections, foundational → behavior → task/schedule/trigger → principal) and applies each collection's batched mutations directly against `ConfigAccess::execute`. A failure midway leaves the DB in a partial state visible to every other observer (agent runtime watcher, P2P peers reading local CRDT state, ad-hoc operator queries). T-Conv (`Proofs/ApplyReconcile/Convergence.lean`) assumes apply runs to completion; today, nothing enforces that assumption from the outside.

This spec wraps the entire apply sequence in a single DefraDB transaction. DefraDB already exposes first-class transactions at both backends the CLI talks to: the HTTP server (`POST /api/v0/tx/begin` plus the `x-defradb-tx` header on every GraphQL request, `POST /api/v0/tx/{id}` to commit, `DELETE /api/v0/tx/{id}` to discard) and the embedded node (`QueryExecutor::begin_txn / execute_in_txn / commit_txn / rollback_txn`). The work is to thread a transaction handle through `apply_desired_state_changes` and its per-collection writers, drive commit-or-discard at the top level, and fence the new atomicity property with a conformance test extension. No Lean changes ship in this PR.

## Why this scope

The PROMPT for #56 names three candidate recovery models: rollback, two-phase commit, and write-ahead log. The brainstorm settled on **DefraDB-native single transaction** (the "rollback" framing, but mechanically implemented via the DB's own transaction primitive rather than an application-level compensating log). Reasoning summarized:

- It is the only candidate that delivers **atomic visibility** to external observers. WAL gives crash recovery but does not hide mid-apply partial state. Application-level compensating-log rollback would race with other writers and can't undo creates without delete semantics (which #57 owns).
- It uses an existing, well-tested DB primitive instead of building parallel infrastructure. The HTTP `tx` endpoints have been in defra-node since well before this work; `EmbeddedNode.runner` is `Arc<dyn QueryExecutor>` and already implements `begin_txn / execute_in_txn / commit_txn / rollback_txn`.
- It survives process crashes trivially: an open transaction held by a CLI process that dies is dropped by DefraDB without committing, so external state stays at the pre-apply snapshot. No on-disk WAL is required because no in-flight apply ever produces externally-observable partial state.
- It keeps the Lean surface unwidened. The PROMPT's substrate-discipline rule is explicit on this point. The new transactional semantics are fenced from Rust against existing emitted fields (`prefix_len`, `pre_live`, `expected_after_desired`, `expected_selected_writes`). A Lean follow-up to emit an explicit external-abort projection is filed as a separate issue.

## Design

### `ConfigApplyTxn` and the threading change

A new type `ConfigApplyTxn` represents an open write transaction over either backend. It is the only argument the apply pipeline takes after the top-level CLI command begins one.

```rust
// crates/defra-agent-cli/src/config_writes/mod.rs
pub(crate) enum ConfigAccess {
    Graphql(String),
    Local(EmbeddedNode),
}

impl ConfigAccess {
    pub(crate) async fn execute(&self, query: &str) -> Result<Value> { /* unchanged */ }

    /// Begin a write transaction on the underlying backend.
    pub(crate) async fn begin_apply_txn(&self) -> Result<ConfigApplyTxn<'_>>;
}

pub(crate) struct ConfigApplyTxn<'a> {
    access: &'a ConfigAccess,
    handle: TxnHandle,
}

enum TxnHandle {
    Graphql(String),               // numeric txn_id parsed from POST /api/v0/tx/begin
    Local(defra_node::TransactionHandle),
}

impl<'a> ConfigApplyTxn<'a> {
    /// Execute a GraphQL query within this transaction.
    /// Graphql path:  POST with `x-defradb-tx: <id>` header.
    /// Local path:    runner.execute_in_txn(req, &handle).
    pub(crate) async fn execute(&self, query: &str) -> Result<Value>;

    /// Commit the transaction. After this, the apply is durable.
    pub(crate) async fn commit(self) -> Result<()>;

    /// Discard the transaction. Returns the underlying error if discard fails;
    /// the top-level orchestrator is responsible for logging and swallowing it
    /// so the original apply error remains the one surfaced to the operator.
    pub(crate) async fn discard(self) -> Result<()>;
}
```

The non-write `ConfigAccess::execute` path remains for code that runs **before** the tx opens — manifest validators, diff queries, GraphQL schema introspection. Those read pre-apply committed state, which is correct: the diff is computed against the live snapshot the manifest is targeting.

Threading: `apply_desired_state_changes`, `apply_import_collection`, `apply_generic_import_collection_batched`, `apply_custom_override_collection_batched`, `apply_custom_override_documents_individually`, `query_existing_documents_by_unique_values`, and the three custom writers in `crates/defra-agent-cli/src/config_writes/` (`write_task_document`, `write_schedule_document`, `write_event_trigger_document`) all change their access parameter from `&ConfigAccess` to `&ConfigApplyTxn`. The pre-write existence probes that determine create-vs-update branches in the custom-writer path must read through the same transaction, otherwise they observe stale pre-tx state and pick the wrong branch. The type change makes that enforced at compile time rather than discipline.

### Top-level orchestrator

The CLI's `config apply` command path drives the braid:

```rust
let txn = access.begin_apply_txn().await?;
let counts = match apply_desired_state_changes(&txn, &desired_bundle, &planned).await {
    Ok(counts) => counts,
    Err(error) => {
        if let Err(discard_err) = txn.discard().await {
            tracing::warn!(%discard_err, "config apply: tx discard failed after apply error");
        }
        return Err(error);
    }
};
if let Err(commit_err) = txn.commit().await {
    return Err(commit_err).context("config apply: commit failed");
}
Ok(counts)
```

(Bound `counts` in the success arm; explicit `commit` block keeps the success/failure paths visually parallel for a scan-read.)

Properties of the braid:

- **Any error from any write aborts the whole apply.** No per-collection commits, no partial successes. Inner per-mutation retry loops in `defra-agent`'s retry classification are unchanged; what changes is that an exhausted retry discards the entire apply rather than continuing past the failure.
- **Apply error wins.** A `discard` failure after an apply error is logged, not surfaced. The user needs to act on the apply error; the transaction will be reclaimed by DefraDB.
- **Commit failure is propagated** as an apply error. From the user's perspective a commit failure is operationally identical to any other apply failure: the DB stayed at pre-apply state.
- **Per-collection progress logging is unchanged.** The existing tracing lines that emit one entry per collection apply remain; transactional wrapping does not alter the user-visible progress surface.

### Backend error asymmetry for `discard`

Discard semantics differ between the two backends and the implementation should not paper that over:

- **Embedded.** `runner.rollback_txn(handle)` returns `TransactionError` only in pathological cases (handle already finalized, lock poisoned). The underlying `db_txn` is dropped in any case. `discard` on the embedded path is effectively infallible from the operator's perspective.
- **HTTP.** `DELETE /api/v0/tx/{id}` is a network call. It can fail for network reasons unrelated to the apply error — connection reset, server restart, timeout. Even if the DELETE never reaches the server, DefraDB's tx GC will reclaim the handle on its own.

Both variants return `Result<()>` so the orchestrator can log the discrepancy when it occurs (useful telemetry). Neither return changes operator-facing behavior: the apply error is what surfaces, and the DB state ends at pre-apply regardless of whether the explicit `discard` round-trip succeeded.

### Crash semantics

The CLI process owns the transaction handle. If the process dies:

- HTTP backend: the TCP connection drops, no commit is ever POSTed, DefraDB's tx GC reclaims the handle.
- Local embedded backend: `DbTransactionContext` is dropped without `force_commit`, the underlying `db_txn` is dropped, the writes never land.

Either way, the next observer of the DB sees the pre-apply snapshot. The operator re-runs `config apply` against the same manifest and the existing prefix-retry semantics (already proven in `prefix_retry_convergence_idempotence`) converge. No on-disk recovery state, no `apply --recover` subcommand, no WAL.

### Concurrent applies

DefraDB uses optimistic concurrency at the storage layer. If two `config apply` invocations race against the same DB, at least one wins; the other's commit fails with a conflict error and the apply braid surfaces that as an apply failure (and discards). The CLI does not serialize invocations at the agent layer — that would require cross-process coordination and is unnecessary given DB-level conflict detection.

## Conformance fence (Lean unwidened)

### Extended production-boundary consumer

`crates/defra-agent-cli/src/config_import.rs::lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary` is extended with a second sub-flavor per Lean case whose `prefix_len > 0` (cases `prefix_retry_convergence_idempotence` with `prefix_len = 1`, `referrer_closure` with `prefix_len = 4`, `production_write_boundary_all_collections` with `prefix_len = 6`).

The injected-failure flavor:

1. Stand up the existing recording GraphQL test server in a "fail at write N" mode, where N = `prefix_len + 1` from the Lean case.
2. The recording server tracks two state windows: writes-inside-the-active-tx (recorded but uncommitted) and committed state. It implements `POST /api/v0/tx/begin`, `POST /api/v0/tx/{id}` (commit), `DELETE /api/v0/tx/{id}` (discard), and honors `x-defradb-tx` on `/graphql` requests for routing into the in-tx window.
3. Run `apply_desired_state_changes` through the new `ConfigApplyTxn` braid against the failing endpoint.
4. Assert:
   - The braid returned `Err`.
   - The observed mutation sequence matches Lean's `expected_selected_writes[..prefix_len + 1]` (no extra writes after the failing one).
   - Exactly one `begin` and one `discard` were recorded on the tx endpoints; **zero** `commit`.
   - Externally-observed committed state equals `pre_live`. This is the red→green pivot — today, without a transaction, partial writes are visible after the failure; with the new braid, the discard reverts them.

The success-path flavor (already implemented today) keeps the existing assertions and adds:
- Exactly one `begin` and one `commit`; zero `discard`.
- Externally-observed committed state matches Lean's `expected_after_desired` projected through the same `LiveState` mapping the test already uses.

Cases with `prefix_len = 0` (`empty_manifest`, `update_existing_backend`, `live_only_no_op`) skip the injected-failure flavor; there is no meaningful interior failure point. The success-path flavor still runs for every case.

#### Recording-server extension — scope note

This is meaningfully more than a tweak to the existing recorder. The current `RecordingGraphqlState` is a single mutation log behind a regex parser, with only the `/` route. The tx-aware extension is roughly:

- Three new routes (`POST /api/v0/tx/begin`, `POST /api/v0/tx/{id}`, `DELETE /api/v0/tx/{id}`) returning numeric ids and `204`-style success/failure on commit/discard.
- A header-routed dispatch on the existing `/` (or `/api/v0/graphql`, whichever the production CLI calls) that reads `x-defradb-tx` and steers the write into the per-tx window vs. committed state.
- A per-tx state struct keyed by id, plus the "committed" snapshot it merges into on `commit`.
- A "fail at write N within tx M" injection knob.

The regex-based mutation parser stays — header routing is at the HTTP layer and does not need a real GraphQL parser. The implementation plan calls this out as its own task (estimated 150–250 LOC, with focused unit tests against the recorder itself before any apply-side test consumes it). Doing the recorder before the production threading change keeps the red→green pivot clean: write the new conformance assertions first, watch them go red against today's production, then turn green as the threading lands.

### Reference-model consumer

`crates/defra-agent/tests/apply_conformance.rs` is unchanged. It consumes the same Lean rows against `defra_agent::apply_model`, which is a pure reference implementation with no DB and no transaction semantics. It stays green by construction.

### Targeted integration test

A new file `crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs` exercises the braid against a real DefraDB node, not the mock recorder. The earlier draft of this section was vague about *how* the apply fails; pinning the mechanism here so the implementer is not chasing "why doesn't my failing manifest fail at the right step."

**Mechanism: mid-apply process termination.** The test exercises the real-node crash recovery path:

1. Build a manifest with at least three documents in each foundational collection (InferenceBackend, InferenceProfile, ToolSelection) plus enough downstream behaviors/tasks/schedules/triggers to make the apply visibly multi-step. The goal is a non-trivial write sequence on the wire so the kill can land mid-stream.
2. Spawn `defra-agent-cli config apply` as a subprocess, pointed at the real DefraDB node via the `Graphql` backend.
3. Tail the node's `Update` event stream (via the existing event-bus subscription used elsewhere in tests) and count incoming document-create events. Once at least one but fewer than all expected create events have arrived, SIGKILL the CLI subprocess.
4. Wait for DefraDB's tx GC to reclaim the orphaned transaction (poll-on-query is enough — querying for the committed state directly).
5. Query the node for every operator-controlled collection. Assert **zero new rows** across every collection that the manifest would have added. SIGKILL between `begin` and `commit` means no write the CLI made in-tx ever becomes committed.

This is more operationally meaningful than synthesizing a manifest that fails apply-time validation: it exercises the real tx-GC path on a real node, which is the actual failure mode operators will hit (`Ctrl-C`, OOM kill, ssh disconnect, container restart).

**Why not a fail-at-write-N mechanism against the real node?** It would require either (a) a passthrough proxy that's a second copy of the recording-server extension above, or (b) deliberately constructing a manifest that passes pre-apply validation but trips a server-side constraint at a specific step — which is brittle (validators and DefraDB GraphQL type checks both move). The recording-server flavor of the conformance fence already covers fail-at-write-N. The integration test's job is to verify the real tx GC behaves as the design assumes.

The existing `cli_config_apply_order.rs` covers prefix-safe ordering and is orthogonal to atomicity — it stays as-is.

### Lean is not widened

The fence is expressed entirely in terms of fields already emitted by `apply_reconcile_cases`: `prefix_len`, `pre_live`, `pre_desired`, `expected_selected_writes`, `expected_after_desired`. No new Lean fields are added in this PR. A follow-up issue (see below) is filed for an explicit `expected_external_state_after_abort` Lean field that would let the assertion become a direct equality rather than a Rust-side restatement.

## Out of scope

- **Delete semantics.** #57 owns this. The transactional braid contains only create/update writes today, mirroring the Lean `ApplyStep` enum. When #57 lands, deletes participate in the same transaction by extension; no rework needed.
- **Reference-model `apply_conformance.rs`.** Stays green and unchanged.
- **P2P-side apply.** This is local CLI apply.
- **Lean surface widening.** Per substrate discipline, filed as a follow-up issue rather than landed here.
- **Concurrency serialization across CLI invocations.** DefraDB's optimistic concurrency is sufficient.

## Follow-up issues to file

1. **Lean: explicit external-abort projection for `apply_reconcile_cases`.** Emit `expected_external_state_after_abort` (= `pre_live` in every current case) so the production-boundary consumer asserts a direct equality. Becomes more interesting when #57 introduces delete semantics. Filed against the ApplyReconcile spec, not blocking #56.
2. **defra-node tx idle-timeout audit.** Confirm the upper bound on an open transaction; either document it or extend it if a real `config apply` could plausibly hit it. Filed only if implementation surfaces a concrete gap.

## Operator-facing change

`defra-agent-cli config apply` is now atomic: any failure during apply leaves the database at the pre-apply snapshot, with no operator cleanup required. Re-running `config apply` against the same manifest after fixing the underlying error converges as before. CHANGELOG entry and release notes updated accordingly.

## Deliverables

- [ ] `ConfigApplyTxn<'a>` and `TxnHandle` defined in `crates/defra-agent-cli/src/config_writes/mod.rs`, with backend-asymmetric discard semantics noted in doc comments.
- [ ] `ConfigAccess::begin_apply_txn` implementations for both `Graphql` (POST `/api/v0/tx/begin`, parse numeric id) and `Local` (`runner.begin_txn(false)`) variants.
- [ ] Threading change: `&ConfigAccess` → `&ConfigApplyTxn` through `apply_desired_state_changes`, `apply_import_collection`, `apply_generic_import_collection_batched`, `apply_custom_override_collection_batched`, `apply_custom_override_documents_individually`, `query_existing_documents_by_unique_values`, and the three `write_*_document` custom writers.
- [ ] Top-level CLI command path braid: `begin_apply_txn` → `apply_desired_state_changes` → `commit` (success) | `discard` (error). Per-collection progress logging preserved.
- [ ] Recording-server extension (its own task): three new tx routes, header-routed dispatch on the GraphQL endpoint, per-tx state window keyed by id, fail-at-write-N injection knob, focused recorder-only unit tests.
- [ ] Extend `generated_apply_reconcile_cases_fence_production_apply_write_boundary` with the injected-failure / no-commit / pre-live-preservation flavor against the extended recorder, plus the success-path `begin`/`commit` count assertion.
- [ ] New `crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs` integration test against a real DefraDB node, exercising the SIGKILL-mid-apply / tx-GC path.
- [ ] Two follow-up issues filed (Lean external-abort projection; defra-node tx idle-timeout audit if implementation surfaces a gap).
- [ ] CHANGELOG / release-note entry.

## Verification

- `cargo fmt --all`
- `cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens`
- `cargo test -p defra-agent-cli` — full green, including the extended production-boundary consumer and the new transactional-rollback integration test.
- `cargo test -p defra-agent --lib --tests` — full green; reference-model `apply_conformance.rs` continues to pass unchanged.
- `cd crates/defra-agent/proofs && lake build` — zero `sorry`s. (No Lean changes in this PR; this is the sanity check.)
