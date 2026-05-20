# DefraDB tx idle-timeout audit for config apply

Date: 2026-05-20

Branch: `audit/issue-226-defradb-tx-idle-timeout`

Issue: <https://github.com/sourcenetwork/defra-agent/issues/226>

Scope: `defra-agent config apply` transactions over the embedded DefraDB node
and the HTTP GraphQL path, using the pinned DefraDB Rust dependency
`defradb.rs` tag `v0.13.1` / rev `2e1a0bfc2c3baa2d2b48d4d5ad09921fbd27378d`.

## TL;DR

I did not find a DefraDB transaction idle timeout that can currently reclaim a
live `config apply` transaction mid-apply in the pinned Rust backend.

The DefraDB transaction registry does have
`cleanup_stale_transactions(max_age)`, but it is a manual API, not a configured
server loop in the code paths used here. It also keys staleness off transaction
creation time, not "idle since the last query". A scheduled caller with a small
`max_age` would therefore be an age cap, not an idle cap, but no production
caller was found in the pinned backend.

The practical timeout envelope for `config apply` is instead:

- Defra-agent HTTP client timeout: 30 seconds per HTTP request to begin,
  commit, discard, or execute a GraphQL mutation in the transaction.
- Defra-agent in-transaction GraphQL retry policy: up to 5 attempts with 100 ms
  scaled backoff for retryable transport/status/decode failures.
- DefraDB query execution timeout: 30 seconds by default per query execution.
- DefraDB HTTP server request timeout: 300 seconds by default per request.

Measured against a real local `defra-agent server`, even a synthetic 1,000
tasks + 1,000 schedules apply with 100 ms proxy delay on every HTTP request
kept the transaction open for about 5.3 seconds and issued 42 in-transaction
GraphQL requests. That is well below the current per-request timeouts. No
chunking or timeout extension is justified by the evidence in this audit.

Recommended follow-up is to correct the stale "DefraDB tx GC" wording in
`ConfigApplyTxn` comments and the SIGKILL rollback test, or to add an explicit
orphan-transaction cleanup policy if DefraDB wants leaked HTTP transactions to
be reclaimed automatically.

## Version and Source Inventory

Defra-agent pins DefraDB Rust crates at tag `v0.13.1` in the workspace
dependencies (`Cargo.toml:40-56`). `Cargo.lock` resolves those dependencies to
rev `2e1a0bfc2c3baa2d2b48d4d5ad09921fbd27378d`.

The local dependency checkout audited for DefraDB internals was:

`~/.cargo/git/checkouts/defradb.rs-4ab0524bccc74f29/2e1a0bf`

The requested crate-local `crates/defra-agent/CLAUDE.md` file does not exist in
this checkout. I used the repository root `CLAUDE.md`.

## Transaction Boundary in config apply

`config apply` does not hold the transaction while doing live validation,
desired-state export, live-state export, or diff planning. The transaction
begins only after those steps:

- Live validation runs first (`crates/defra-agent-cli/src/commands/config/apply.rs:43-54`).
- Desired and live bundles are built and diffed before the transaction
  (`crates/defra-agent-cli/src/commands/config/apply.rs:56-68`).
- The write transaction begins at
  `crates/defra-agent-cli/src/commands/config/apply.rs:70-74`.
- Only `apply_desired_state_changes` runs inside the transaction
  (`crates/defra-agent-cli/src/commands/config/apply.rs:75-85`).
- Commit runs immediately after the write phase
  (`crates/defra-agent-cli/src/commands/config/apply.rs:87-89`).
- The post-commit verification diff runs outside the transaction
  (`crates/defra-agent-cli/src/commands/config/apply.rs:93-102`).

The write phase itself walks the fixed 9-collection apply order:
`InferenceBackend`, `InferenceProfile`, `ToolServiceRegistry`,
`ToolSelection`, `AgentBehavior`, `Task`, `Schedule`, `EventTrigger`,
`AgentPrincipal` (`crates/defra-agent-cli/src/config_import.rs:36-46`).

Aliased mutations are batched at 50 mutation fields per GraphQL request
(`crates/defra-agent-cli/src/config_import.rs:28` and
`crates/defra-agent-cli/src/config_import.rs:514-525`). The transactional
rollback test hook `DEFRA_AGENT_CONFIG_APPLY_SLEEP_MS` sleeps after each
collection during the write phase
(`crates/defra-agent-cli/src/config_import.rs:598-617`).

## Timeout Inventory

### Defra-agent HTTP transaction client

For HTTP GraphQL mode, `ConfigApplyTxn` creates a reqwest client with a
30-second timeout for `POST /api/v0/tx`
(`crates/defra-agent-cli/src/config_writes/txn.rs:162-199`). The same client is
stored on the transaction object and used for commit and discard:

- Commit: `POST /api/v0/tx/{id}`
  (`crates/defra-agent-cli/src/config_writes/txn.rs:85-112`).
- Discard: `DELETE /api/v0/tx/{id}`
  (`crates/defra-agent-cli/src/config_writes/txn.rs:122-150`).

In-transaction GraphQL requests use `execute_graphql_async_with_tx` with a
30-second reqwest timeout, `max_attempts = 5`, and 100 ms retry backoff
(`crates/defra-agent-cli/src/config_writes/txn.rs:56-72`). The shared GraphQL
helper creates a reqwest client with that timeout and retries retryable
transport/status/decode errors until attempts are exhausted
(`crates/defra-agent-protocol/src/graphql.rs:190-203` and
`crates/defra-agent-protocol/src/graphql.rs:233-330`).

Implication: the client-side cap is per request attempt, not per whole apply.
A retry storm can extend transaction-open wall time beyond 30 seconds, but any
single HTTP attempt is capped at 30 seconds by the client.

### Defra-agent embedded/local transaction path

For local embedded mode, `ConfigApplyTxn` calls
`runner.begin_txn(false)`, then `execute_in_txn`, `commit_txn`, or
`rollback_txn` directly
(`crates/defra-agent-cli/src/config_writes/txn.rs:73-80`,
`crates/defra-agent-cli/src/config_writes/txn.rs:113-117`, and
`crates/defra-agent-cli/src/config_writes/txn.rs:201-212`). There is no
defra-agent reqwest timeout on this in-process path. The relevant cap is the
DefraDB query runner timeout.

### DefraDB query and HTTP server timeouts

In the pinned DefraDB backend, `QueryRunner` defaults query execution timeout
to 30 seconds and exposes `with_query_timeout`
(`defradb.rs@2e1a0bf/crates/query/src/runner/mod.rs:193-214` and
`defradb.rs@2e1a0bf/crates/query/src/runner/mod.rs:351-354`). The executor
wraps query execution with that timeout
(`defradb.rs@2e1a0bf/crates/query/src/runner/executor.rs:243` and
`defradb.rs@2e1a0bf/crates/query/src/runner/executor.rs:450-473`).

DefraDB HTTP `ServerConfig` defaults request timeout to 300 seconds and applies
`tower::timeout::TimeoutLayer`
(`defradb.rs@2e1a0bf/crates/http/src/server.rs:44-60` and
`defradb.rs@2e1a0bf/crates/http/src/server.rs:457-475`).

Standalone `defradb start` exposes both knobs:

- `--request-timeout`, default 300 seconds
  (`defradb.rs@2e1a0bf/crates/cli/src/commands/start/mod.rs:139-141` and
  `defradb.rs@2e1a0bf/crates/cli/src/commands/start/mod.rs:454-455`).
- `--query-timeout`, default 30 seconds
  (`defradb.rs@2e1a0bf/crates/cli/src/commands/start/mod.rs:183-185` and
  `defradb.rs@2e1a0bf/crates/cli/src/commands/start/mod.rs:487-488`).

`defra-agent server` does not expose those DefraDB timeout knobs today.
`ServeArgs` has `home`, data dir, HTTP bind, identity, tool, and P2P options,
but no request/query timeout options
(`crates/defra-agent-cli/src/cli/args.rs:296-333`). The server constructs
`defra_node::HttpConfig::with_addr(...).with_extra_routes(...)`
(`crates/defra-agent-cli/src/commands/serve.rs:108-115`), and DefraDB's
`HttpConfig` only carries address and extra routes
(`defradb.rs@2e1a0bf/crates/defra-node/src/config.rs:21-48`). `defra-node`
then builds `defra_http::ServerConfig { address, query_limits, ..Default::default() }`,
which leaves request timeout at 300 seconds
(`defradb.rs@2e1a0bf/crates/defra-node/src/lib.rs:706-715`). Its query runner
sets query limits but does not call `with_query_timeout`, so the 30-second
default remains
(`defradb.rs@2e1a0bf/crates/defra-node/src/lib.rs:846-852`).

## Transaction Cleanup / "Idle Timeout" Finding

The pinned DefraDB transaction context records `created_at: Instant` when the
transaction context is created
(`defradb.rs@2e1a0bf/crates/db/src/txn_context.rs:30` and
`defradb.rs@2e1a0bf/crates/db/src/txn_context.rs:44-58`).

The registry exposes `cleanup_stale_transactions(max_age)` and treats a
transaction as stale when `now.duration_since(ctx.created_at()) > max_age`
(`defradb.rs@2e1a0bf/crates/db/src/txn_registry.rs:252-264`). Cleanup removes
the registry entry and force-discards the transaction
(`defradb.rs@2e1a0bf/crates/db/src/txn_registry.rs:268-300`).

That function is not idle-based; activity inside the transaction does not
refresh `created_at`. If a scheduler called it with a small `max_age`, it could
discard a long-running but active transaction. However, searching the pinned
`crates/{db,cli,http,defra-node,query}/src` tree found callers only in tests
and comments, not in the production server/node path.

Normal transaction finalization removes the handle from the registry:

- `begin` inserts the context
  (`defradb.rs@2e1a0bf/crates/db/src/txn_registry.rs:617-671`).
- `commit` removes the context then force-commits it
  (`defradb.rs@2e1a0bf/crates/db/src/txn_registry.rs:691-720`).
- `rollback` removes the context before rolling it back
  (`defradb.rs@2e1a0bf/crates/db/src/txn_registry.rs:723-730`).

Conclusion: the current `config apply` risk is not "DefraDB tx idle timeout
reclaims a slow but live apply". The risk is bounded by per-request client,
query, and HTTP server timeouts. Separately, orphaned HTTP transaction handles
can be leaked if the client disappears before commit/discard and no explicit
cleanup caller exists.

## Measurement Harness

I measured real `defra-agent config apply` runs against a real local
`defra-agent server`, using `target/debug/defra-agent` built from this checkout.
The harness was intentionally out-of-tree and did not change the repository.

Harness shape:

- Start a mock OpenAI-compatible model endpoint for `defra-agent init`.
- Start `defra-agent server`.
- Place an HTTP proxy in front of `/api/v0/graphql` and `/api/v0/tx`.
- Record monotonic timestamps for `POST /api/v0/tx` begin and
  `POST /api/v0/tx/{id}` commit.
- Count GraphQL requests carrying `x-defradb-tx`.
- Optionally sleep 100 ms before each proxied HTTP request.
- Generate a baseline exported config, then add synthetic Task and Schedule
  documents referencing the default behavior.
- Run `config apply --graphql <proxy>/api/v0/graphql`.

The "tx open" column below is conservative: begin request start through commit
response completion. The transaction is not usable until the begin response is
returned, so the true usable transaction window is slightly smaller.

| Synthetic changed docs | Proxy delay | Command wall | Tx open | In-tx GraphQL requests |
| ---: | ---: | ---: | ---: | ---: |
| 1 task + 1 schedule | 0 ms/request | 57.9 ms | 8.5 ms | 4 |
| 1 task + 1 schedule | 100 ms/request | 2442.9 ms | 676.5 ms | 4 |
| 200 tasks + 200 schedules | 0 ms/request | 184.5 ms | 105.3 ms | 10 |
| 200 tasks + 200 schedules | 100 ms/request | 3172.9 ms | 1400.0 ms | 10 |
| 1000 tasks + 1000 schedules | 0 ms/request | 1372.6 ms | 1138.4 ms | 42 |
| 1000 tasks + 1000 schedules | 100 ms/request | 7249.4 ms | 5295.3 ms | 42 |

All measured runs applied successfully and reported the expected Task and
Schedule counts. The 1,000 + 1,000 document case issued 42 in-transaction
GraphQL requests because the 50-field batch size creates 20 Task batches, 20
Schedule batches, and a small number of baseline document mutations.

## Safe Envelope

For current defaults and measured behavior:

- Transaction-open wall time is dominated by the number of in-transaction HTTP
  requests times network/proxy delay plus DefraDB execution time.
- The measured 2,000 synthetic-doc apply with 100 ms/request proxy delay stayed
  around 5.3 seconds transaction-open wall time.
- The relevant hard cap is per request/attempt, not total transaction duration:
  a single slow mutation can fail at the 30-second client timeout or the
  30-second DefraDB query timeout even if total apply duration is otherwise
  reasonable.
- The DefraDB HTTP server default request timeout is 300 seconds, so it is not
  the first cap for default HTTP config apply. The defra-agent client and
  DefraDB query runner 30-second defaults are tighter.
- Because no production transaction cleanup scheduler was found, there is no
  observed tx-age or tx-idle cap that forces chunking before commit.

A rough sizing formula for HTTP mode is:

`tx_open ~= begin + commit + ceil(changed_docs_per_collection / 50) requests per changed collection + fixed baseline requests`

Operators should remeasure if they expect:

- tens of thousands of changed config documents in a single apply;
- mutation executions approaching 30 seconds each;
- high packet loss or proxies that trigger retryable transport failures;
- request/response latency in the multi-second range.

Those conditions could hit the fixed 30-second client/query request caps even
though there is no transaction idle timeout.

## Failure Semantics if a Timeout Fires

If an in-transaction mutation returns an error or times out, `config apply`
attempts to discard the transaction and returns the original apply error
(`crates/defra-agent-cli/src/commands/config/apply.rs:75-85`).

If discard itself fails in HTTP mode, the original error still surfaces, and
the server-side transaction handle can remain orphaned because no automatic
cleanup caller was found. The transaction's uncommitted writes are not visible
as committed state, but the leaked handle is a resource-management risk.

If commit returns an error, `config apply` reports
`config apply: commit failed`
(`crates/defra-agent-cli/src/commands/config/apply.rs:87-89`). A commit request
that times out is inherently ambiguous: the server may have committed after the
client stopped waiting. This is not specific to DefraDB and was not observed in
the measurements, but operators should treat a commit-time timeout as requiring
post-failure inspection.

## Test Wording Gap

The transactional rollback test says it waits for "DefraDB's tx GC" to reclaim
an orphaned transaction
(`crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs:17-20`
and
`crates/defra-agent-cli/tests/cli_config_apply_transactional_rollback.rs:103-104`).

Given the pinned backend, that wording overstates what the test proves. The
test kills a CLI mid-apply and then polls row counts until they equal the
pre-apply snapshot. Because uncommitted transaction writes are not externally
visible, the poll can pass without proving that a server-side orphaned handle
was cleaned up. The test is still useful as an atomicity/visibility regression
test, but it is not evidence of automatic transaction garbage collection.

The same stale assumption appears in the `ConfigApplyTxn` module comment:
"Even if the DELETE never reaches the server, DefraDB's tx GC will reclaim the
handle on its own" (`crates/defra-agent-cli/src/config_writes/txn.rs:8-18`).

## Recommendation

Do not extend or chunk `config apply` only for a presumed DefraDB tx idle
timeout. In the pinned backend audited here, that timeout does not appear to
exist as an active production mechanism.

Recommended follow-ups:

1. Update the `ConfigApplyTxn` comment and transactional rollback test comment
   so they claim atomicity/visibility, not automatic tx GC.
2. File a DefraDB/defra-agent follow-up to add an explicit orphaned HTTP
   transaction cleanup policy if leaked handles after crashed clients matter
   operationally.
3. Consider exposing DefraDB `request_timeout` and `query_timeout` through
   `defra-agent server` if deployments need to tune large or remote config
   applies. Standalone `defradb start` already exposes these knobs.
4. Keep the existing 50-field mutation batch size unless future measurements
   show individual mutations approaching the 30-second query/client timeout.
