# Incremental ObservedStore loading

Design for [`defra-agent#62`](https://github.com/sourcenetwork/defra-agent/issues/62) — *ObservedStore loads full snapshot on every update instead of incremental entries*.

Status: design (no implementation yet).
Owners: desktop client / observation path.
Related work: [`defradb.rs#943`](https://github.com/sourcenetwork/defradb.rs/issues/943) (durable subscription cursor — filed alongside this design and decoupled from it).

## 1. Problem and current state

### 1.1 What the issue claims

The observation path calls `load_full_snapshot()` on every `EventName::Update` event from `DefraNode.subscribe()`. For a session with a long history, every token update reloads the entire compacted history; cost grows with token rate × history size.

### 1.2 Code as it actually exists today

The issue's filename (`observer.rs`) is slightly off; the real file is `crates/defra-agent-desktop-core/src/client/observe.rs`. Validated entry points:

- **`spawn_observer`** in `client/observe.rs:96`. Subscribes to `EventName::Update`, debounces 150 ms, drains `try_recv()`, and calls `load_full_snapshot(node)` on every flush — `client/observe.rs:131`.
- **`ClientCore::refresh_store`** in `client/core.rs:275`. Operator-initiated full refresh; calls `load_full_snapshot`.
- **`ClientCore::refresh_remote_peer_record`** in `client/core.rs:309`. Per-peer refresh; calls `load_full_snapshot_from_graphql`.
- **`spawn_materialization_supervisor_task`** in `client/core/materialization.rs:120`. After a stalled-materialization repair, reloads the full snapshot.
- **`bootstrap`** in `client/core/bootstrap.rs:82`. Calls `load_full_snapshot_with_peer_records` once at startup.

`load_full_snapshot` itself (`client/query.rs:19`) issues 18 GraphQL queries — every collection the desktop knows about, every row of every collection — and `load_full_snapshot_from_graphql` does the same against a remote peer's GraphQL endpoint.

### 1.3 What DefraDB's subscription API actually provides

Pinned `defradb.rs` rev `25b935b`. Source of truth for what events carry:

```rust
// crates/events/src/event.rs
pub struct Update {
    pub doc_id: String,
    pub subject_doc_id: Option<String>,
    pub cid: Cid,
    pub collection_id: String,    // schema version ID, stable
    pub block: Bytes,
    pub is_retry: bool,
    pub is_relay: bool,           // true = arrived via P2P; false = local mutation
}
```

Key facts:

- **No sequence number, no replay cursor.** Subscriptions are live-only.
- **`subscribe(&[EventName])` is the only entry point.** No per-collection topic, no `subscribe_after(seq)`.
- **Bounded mpsc with drop counter.** `Subscription::check_and_reset_dropped()` reports messages dropped due to buffer overflow.
- **Existing prior art:** `agent/runtime/control_watcher.rs:31` opens a subscription, applies per-event control updates, and on drop falls back to a full reload of the document view. The pattern this design uses is the same shape.

The issue's proposed `node.subscribe_after(Collection, last_seq)` API does not exist on DefraDB today and is not currently on any roadmap. `defradb#4617` (cursor-based query pagination) is *query-side* pagination, not subscription replay; useful for paginated initial fetches but does not let an observer resume an event stream. We filed [`defradb.rs#943`](https://github.com/sourcenetwork/defradb.rs/issues/943) to track the upstream feature; this design is decoupled from it.

### 1.4 What `merge_snapshot` already gives us

`ClientStore::merge_snapshot` (`client/store/mod.rs:136`) is **upsert by stable per-collection key** for every replicated collection. A `ClientStore` containing a single changed row merges into the existing store correctly without disturbing the rest. We do not need to invent merge semantics — we need to feed `merge_snapshot` smaller patches.

## 2. Goals and non-goals

### Goals

- Eliminate `load_full_snapshot` from the per-event hot loop.
- Cap reload blast radius. When a reload is necessary, scope it to the currently-selected agent rather than the entire fleet of replicated agents.
- Lazy re-fetch on agent/session selection switch.
- Preserve client-derivation properties currently captured in `Proofs/Client.lean`.

### Non-goals

- Bootstrap pagination of long transcripts. Load-bearing for sessions with thousands of messages, but gated on a Rust port of `defradb#4617`. Tracked as a follow-up.
- A real subscription cursor. Tracked in `defradb.rs#943`; if/when landed, this design's drop-recovery path collapses to a cleaner cursor resume.
- Eviction of stale rows from offline peers. Pre-existing posture; unchanged here.
- A new Lean theorem. Existing `deriveTurn` properties are silent on snapshot construction and continue to hold under upsert-by-key incremental merge.

## 3. Design

### 3.1 Components

Three components, all inside `crates/defra-agent-desktop-core/src/client/`:

#### 3.1.1 `CollectionResolver`

`Arc<RwLock<HashMap<String /*collection_id*/, &'static str /*name*/>>>` cache. Lazily resolves on first event for a given `collection_id` by calling `node.get_collection(name)` for each name in `defra_agent_protocol::schemas::ALL_COLLECTION_NAMES` and inverting the resulting `(name → collection_id)` map. Cached for the lifetime of the process — collection IDs are stable.

This is the same pattern as `EventSource::collection_id_to_name` in `trigger_engine/event_source.rs:83`. Same justification, same invalidation rule.

#### 3.1.2 Incremental fetcher

Two new functions in `client/query.rs`:

```rust
pub async fn fetch_doc_patch(
    node: &EmbeddedNode,
    collection_name: &str,
    doc_ids: &[&str],
) -> Result<ClientStore>;

pub async fn load_agent_scoped_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<ClientStore>;
```

`fetch_doc_patch` runs `query { Collection(filter: {_docID: {_in: [<ids>]}}) { …all fields… } }` against the per-collection field list constants already in `query.rs`, falling back to a per-doc loop if `_in` is unsupported on the pinned DefraDB version. Returns a `ClientStore` containing only matched rows.

`load_agent_scoped_snapshot` runs the existing 18 queries with `filter: {agent_did: {_eq: $agent_did}}` on the agent-keyed collections; for transcripts it filters by `session_id` derived from the agent's conversations. Control-plane-only collections (`InferenceBackend`, `InferenceProfile`, `ToolServiceRegistry`, `ToolSelection`) reload in full — they're small and not partitioned by agent.

Both functions reuse `escape_graphql_string` and `load_rows`.

#### 3.1.3 Rewritten `spawn_observer`

Replaces the per-event `load_full_snapshot` with a debounced burst-coalescer:

- Maintains `dirty: HashMap<&'static str /*name*/, HashSet<String /*doc_id*/>>` accumulated during the 150 ms debounce.
- On debounce expiry: for each `(name, doc_ids)` entry, call `fetch_doc_patch`, merge each result into the store with `merge_snapshot`.
- On `subscription.check_and_reset_dropped() > 0`: discard `dirty`, fall back to `load_agent_scoped_snapshot(node, selected_agent_did)` and `merge_snapshot`. If no agent is selected, fall back to `load_full_snapshot` (matches today's behavior — only happens before any agent is selected).
- Reads selection from a new `selected_agent_did_rx: watch::Receiver<Option<String>>` channel owned by `ClientCore`.

The invariant: every code path that previously called `merge_snapshot(load_full_snapshot(...))` ends up calling `merge_snapshot` with a smaller `ClientStore` patch. Since `merge_snapshot` is row-upsert by stable key, correctness reduces to *the patch contains every row that changed since the last merge*.

### 3.2 Data flow

#### 3.2.1 Steady-state per-event path

```
DefraNode  ─┐
            │ EventName::Update { doc_id, collection_id, is_relay, ... }
            ▼
spawn_observer ── debounce 150ms, drain try_recv() ── dirty: { name → {doc_ids} }
            │
            │ for (name, ids) in dirty:
            ▼
fetch_doc_patch(node, name, ids)  ─►  GraphQL: Collection(filter:{_docID:{_in:[…]}})
            │
            │ ClientStore (only changed rows)
            ▼
ObservedStore::merge_snapshot(patch)  ── upsert by stable key
            │
            │ version_tx.send_replace(v+1)
            ▼
Tauri bridge re-emits "desktop://client-updated"
```

For ~10 streaming token updates landing in the 150 ms window: dirty becomes `{AgentResponse: {response_key}}`, fetch is one single-row query, merge upserts one row. Compared to today: 18 collection queries, every row of every collection.

#### 3.2.2 Drop-recovery path

```
spawn_observer detects drop > 0
            │
            │ discard dirty set (we don't know what we missed)
            ▼
selected_agent_did_rx.borrow().clone()  ── current scope
            │
            ├─ Some(agent_did) ─►  load_agent_scoped_snapshot(node, agent_did)
            │
            └─ None ─►  load_full_snapshot (matches today)
            ▼
ObservedStore::merge_snapshot(scoped_patch)
```

`merge_snapshot` is *additive upsert*, not replace. Rows for non-selected agents stay at whatever they were last merged to. The conversation list keeps showing other agents' summaries; only the selected agent's data is freshly authoritative after recovery.

#### 3.2.3 Selection-switch path

```
Desktop UI: user clicks a different agent
            │
            ▼
selected_agent_did_tx.send_replace(Some(new_agent_did))
            │
            ▼
ClientCore::ensure_agent_loaded(new_agent_did)
            │  if last_loaded_for(new_agent_did) within 2s → no-op
            │  else → load_agent_scoped_snapshot + merge_snapshot
            ▼
ObservedStore (now contains fresh rows for new scope)
```

A `Mutex<HashMap<String /*agent_did*/, Instant /*last_loaded*/>>` on `ClientCore` debounces repeated switches.

#### 3.2.4 Bootstrap path (unchanged)

`ClientCore::bootstrap` continues to call `load_full_snapshot_with_peer_records` once. No prior state to incrementalize from. Bootstrap pagination is a follow-up gated on `defradb#4617`.

#### 3.2.5 Materialization-supervisor refresh

`materialization.rs:120` swaps from `load_full_snapshot` → `load_agent_scoped_snapshot` keyed by the repair target's `agent_did`. Repair already operates on a known `(session_id, request_id)` so the agent is implied.

### 3.3 Error and race handling

#### 3.3.1 Subscribe-time race (pre-existing bug, fixed here)

Today the observer subscribes inside `spawn_observer` *after* `bootstrap` has already issued `load_full_snapshot_with_peer_records`. Any write that lands between bootstrap's read and `subscribe(...)` is lost until the next event in the same collection wakes us up. The current full-reload design accidentally heals this on every event; an incremental design surfaces it.

Fix:

```
bootstrap()
  ├─ subscription = node.subscribe(&[EventName::Update])    // 1. open first
  ├─ snapshot = load_full_snapshot_with_peer_records(...)   // 2. read full state
  ├─ store = ObservedStore::new(snapshot)
  └─ spawn_observer(node, store, subscription)              // 3. drain queued events
```

Events that arrived between (1) and (2) are buffered in the bounded mpsc and drained on first observer tick. They may be redundant — `merge_snapshot` is idempotent. Same shape as `control_watcher.rs:31`.

#### 3.3.2 Doc-not-found at fetch time

A delete event leaves `doc_id` referring to a row that no longer satisfies the query. `fetch_doc_patch` returns zero rows.

- **Phase 1 (this design):** treat zero rows as "no patch needed." The deleted row stays in `ObservedStore` until the next agent-scoped reload evicts it. Soft-delete-by-omission is the existing posture.
- **Phase 2 (follow-up):** detect deletes from the event's `block` payload, or wait for a real delete signal in the events API. Out of scope.

#### 3.3.3 GraphQL fetch failure

Per-doc query fails (network, DB busy, schema mismatch).

- Mark `(collection_name, doc_id)` as deferred with a timestamp.
- Re-attempt at the next debounce tick or next event for that collection.
- After 3 consecutive failures, `warn!` and drop. The doc will be picked up by the next dropped-event recovery or scoped reload.

#### 3.3.4 Concurrent merges

`ObservedStore::merge_snapshot` already takes the write lock and runs to completion; the `version_tx` increment is atomic with the merge. Multiple per-doc patches in the same debounce tick run sequentially under the write lock. The watch channel's `send_replace` collapses bursts naturally for downstream subscribers.

#### 3.3.5 Selection-switch races

User switches A → B → A rapidly. `ensure_agent_loaded`'s `last_loaded_for` debouncer no-ops the second switch back to A within 2 s. If a switch lands mid-fetch, the in-flight fetch completes and merges (idempotent); only the most recent `selected_agent_did` value drives subsequent fetches.

#### 3.3.6 Drop with no selection

If the bus drops events while no agent is selected (agent-list screen), there's no scope to fall back to. Behavior: `warn!`, retain the dropped count, trigger a scoped reload on the next selection. Matches today — the agent-list screen is read from conversation summaries, which a) are small and b) heal on the next selection's scoped reload.

#### 3.3.7 Local writes (`is_relay = false`)

Mutation paths in `client/mutations/` that write through `ObservedStore` directly produce a redundant per-doc fetch + merge when the same write lands as an `is_relay = false` event. Keys are stable, the merge is idempotent — net cost is one extra GraphQL query per local write. Cheap relative to the old per-event full snapshot. A counter `local_write_redundant_fetch_total` lets us tune later (e.g. recently-self-written bloom filter) without speculation now.

## 4. Tests

### 4.1 Unit tests (`client/observe/tests.rs`, new file)

- `coalesces_burst_into_one_fetch_per_doc` — 50 events for the same `(AgentResponse, response_key)` inside the debounce window; assert exactly one GraphQL query is issued.
- `multi_collection_burst_fans_out_correctly` — events across `AgentResponse`, `AgentMessage`, `AgentToolCall`; assert one query per (collection, doc-batch).
- `dropped_events_trigger_scoped_reload` — non-zero drop count via the shared `dropped_count` `AtomicU64`; assert `load_agent_scoped_snapshot` is invoked with the currently-selected `agent_did`, not `load_full_snapshot`.
- `dropped_events_with_no_selection_falls_back_to_full` — same but `selected_agent_did = None`; full reload is used.
- `selection_switch_loads_new_scope_lazily` — switch `agent_did` → assert scoped fetch fires for the new agent and not for the old.
- `selection_switch_debounces_repeats` — A → B → A inside 2 s; assert exactly one fetch per agent.
- `subscribe_before_snapshot_drains_queued_events` — open subscription, write a doc, run bootstrap; assert post-bootstrap merge includes that doc.
- `delete_event_leaves_stale_row` — fetch returns zero rows; assert existing row is preserved and version increments.
- `failed_fetch_retried_then_dropped` — `fetch_doc_patch` errors 3× then succeeds; assert retry, then on 4th persistent failure the pair is dropped with a `warn!` log.
- `local_write_redundant_fetch_counter` — local mutation followed by matching `is_relay=false` event; assert the redundant-fetch counter increments and the row is unchanged after merge.

A `RecordingNode` helper (mirror of `RecordingP2P` in `materialization.rs:333`) instruments GraphQL query dispatch for assertions.

### 4.2 Integration tests (`crates/defra-agent-desktop-core/tests/client_store.rs`)

- `incremental_observer_handles_long_session` — seed 1 000 `AgentMessage` rows + 1 `AgentResponse`. Stream 100 token-update events. Assert (a) total GraphQL rows fetched is independent of the seeded `AgentMessage` count (proportional to debounce-flush count × dirty-doc count, not history size); (b) final store state matches a control run with 100 full snapshots; (c) p99 per-event latency below threshold (set in implementation, not design).
- `agent_scope_isolation` — two agents in the local replica. Drop events while agent A is selected. Assert agent B's rows are untouched after recovery.
- `bootstrap_then_observer_no_lost_writes` — write 10 docs, run bootstrap, write 10 more concurrently with subscribe; assert final store has all 20.

### 4.3 Lean and conformance impact

`Proofs/Client.lean` defines `deriveTurn` over a normalized observation list and is silent on snapshot construction. Monotonicity (T2), terminal coherence (T3), totality (T4), retry replacement (T5) are properties of the derivation function applied to *some* snapshot — they do not depend on whether that snapshot was assembled by full reload or upsert-by-key incremental merge. The T1 merge assumption — *equivalent merged observations converge before derivation* — is exactly what `merge_snapshot`'s upsert-by-stable-key gives us.

**No new theorem.** Add a paragraph to `crates/defra-agent/proofs/client-state-machine.md` documenting the subscription invariant we now rely on:

> A compliant client MUST open its `EventName::Update` subscription before reading the initial snapshot and MUST drain the subscription continuously thereafter. Drop-counter signals MUST trigger a scope-bounded resync. Per-event patches MUST upsert by stable key (the `*_merge_key` functions in `defra-agent-desktop-core::client::store`).

### 4.4 Observability

- `tracing::trace!` per per-doc fetch with `(collection, doc_id, version)`.
- `tracing::debug!` per debounce flush with `(dirty_collections, total_docs, fetch_ms)`.
- `tracing::warn!` on drop > 0 with `(dropped, scope)`.
- `tracing::warn!` on persistent fetch failure with `(collection, doc_id, error)`.
- `ObserverMetrics` struct (atomic counters): `events_received`, `docs_fetched`, `debounce_flushes`, `scope_reloads`, `drop_recoveries`, `local_write_redundant_fetches`. Exposed via `ClientCore::observer_metrics()` and a new desktop diagnostics tauri command.

## 5. Migration plan

One PR per row; each row green before the next opens.

| # | PR scope | Touches | Reversible? |
|---|---|---|---|
| 1 | Add `CollectionResolver` + `fetch_doc_patch` + `load_agent_scoped_snapshot` + tests. **No call sites changed yet.** | `client/query.rs`, new `client/collection_resolver.rs` | trivially — dead code |
| 2 | Add `selected_agent_did: watch::Sender<Option<String>>` on `ClientCore`; expose `set_selected_agent_did()` and `selected_agent_did_rx()`. Wire from desktop bridge but don't act on it yet. | `client/core.rs`, `apps/desktop-tauri/src-tauri/src/bridge/state.rs`, agent-switch tauri commands | trivially — channel with no consumer |
| 3 | Move `node.subscribe(...)` into `bootstrap.rs` to fix the subscribe-time race. Pass the existing `Subscription` into `spawn_observer`. **Behavior unchanged.** | `client/core/bootstrap.rs`, `client/observe.rs` signature | reverse the move |
| 4 | Replace `spawn_observer`'s body with the debounced burst-coalescer + per-doc fetch + scoped drop fallback. Add `client/observe/tests.rs`. **First behavior-changing PR.** | `client/observe.rs`, new tests | revert the file |
| 5 | Replace `materialization.rs:120` with `load_agent_scoped_snapshot`. | `client/core/materialization.rs` | one-line revert |
| 6 | Replace `ClientCore::refresh_store` and `refresh_remote_agent` with their scoped variants. | `client/core.rs` | revert |
| 7 | Add `ensure_agent_loaded` lazy-load on selection switch + `last_loaded_for` debouncer. | `client/core.rs`, bridge `tauri_commands` | revert |
| 8 | Add the conformance paragraph in `client-state-machine.md`. Add `ObserverMetrics` and the diagnostics accessor. | `crates/defra-agent/proofs/client-state-machine.md`, `client/observe.rs` | revert |

`load_full_snapshot` itself stays in `query.rs` — bootstrap still uses it, and it's the bottom of the drop-recovery fallback when no agent is selected. We do not delete code we still call.

Rollout: the desktop app is the only consumer of `defra-agent-desktop-core`. No semver coordination cost. Each PR ships behind no flags — these are corrections, not features.

## 6. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `_docID: {_in: [...]}` unsupported on pinned DefraDB version | Medium | Falls back to a per-doc loop — slower bursts but correct | Phase 1 uses the loop; PR 4 follow-up checks `_in` support and switches if available |
| Soft-delete-by-omission causes ghost rows | Medium | Stale UI for deleted docs until next scoped reload | Documented as a known limitation; tracked under a follow-up; obsolete when DefraDB grows a delete signal |
| `is_relay = false` doubled merges accumulate into measurable CPU | Low | One extra GraphQL query per local write | `local_write_redundant_fetch_total` counter; tune with a recently-self-written bloom filter only if it shows up in profiling |
| Subscribe-before-snapshot pre-load floods the bounded mpsc on a busy node | Low | Drop counter trips immediately, triggers scoped reload | Drop-recovery path is itself tested |
| Per-doc fetches under heavy stream burst saturate the GraphQL executor | Low–Medium | Token streaming UI lags despite the optimization | Benchmark in PR 4 with a 100-tokens-in-150ms scenario; if needed, batch into a single multi-doc query before merging |
| Lean conformance text drifts from runtime behavior | Low | Doc lies about implementation contract | Conformance text added in PR 8 alongside the metrics that prove the contract holds |

## 7. Open questions

1. **Where does `selected_agent_did` live?** *Default:* on `ClientCore`, owned by the bridge state, plumbed to the observer via `watch::Receiver`. Alternative: a global `OnceCell` — rejected on principle.
2. **Selection-switch debounce timeout.** *Default:* hardcode 2 s; revisit only if metrics say so.
3. **Expose `observer_metrics()` over the desktop bridge?** *Default:* yes, behind the existing diagnostics tauri command — costs ~30 lines.

## 8. Out of scope

Tracked here so reviewers know they were considered:

- Bootstrap pagination of long transcripts. Load-bearing for thousands-of-messages sessions; gated on Rust port of `defradb#4617`. Follow-up issue once that lands.
- Subscription cursor / `subscribe_after`. Tracked in `defradb.rs#943`.
- Batched per-doc fetches via `_in` arrays. Implementable today; defer until measurement says it matters.
- Eviction policy for stale rows from offline peers. Pre-existing posture; not made worse by this design.

## 9. Implementation breakdown for codex agents

Each PR below is independently reviewable and matches one row of the migration table in §5. Tests called out per PR are the green-bar gate before the next PR opens.

### PR 1 — Incremental fetch primitives

- Add `client/collection_resolver.rs` with `CollectionResolver::resolve(node, collection_id) -> Option<&'static str>`.
- Add `fetch_doc_patch(node, collection_name, doc_ids: &[&str]) -> Result<ClientStore>` to `client/query.rs`. Delegates to existing per-collection field-list constants. Uses `_docID: {_in: [...]}` with a per-doc fallback if the executor rejects it.
- Add `load_agent_scoped_snapshot(node, agent_did) -> Result<ClientStore>` to `client/query.rs`. Mirrors `load_full_snapshot` but adds `filter: {agent_did: {_eq: $agent_did}}` to agent-keyed collections and filters transcript collections by `session_id ∈ $agent_sessions`. Control-plane-only collections still load in full.
- Tests: `collection_resolver_caches_id_to_name`, `fetch_doc_patch_returns_only_matching_rows`, `fetch_doc_patch_falls_back_when_in_unsupported`, `load_agent_scoped_snapshot_excludes_other_agents`.

### PR 2 — Selection plumbing

- Add `selected_agent_did: watch::Sender<Option<String>>` to `ClientCore` (default `None`).
- Expose `ClientCore::set_selected_agent_did(did: Option<String>)` and `selected_agent_did_rx() -> watch::Receiver<Option<String>>`.
- Wire from `apps/desktop-tauri/src-tauri/src/bridge/state.rs` and from agent-switch tauri commands.
- No consumer reads the channel yet — purely additive.

### PR 3 — Subscribe-before-snapshot

- In `client/core/bootstrap.rs`, open `node.subscribe(&[EventName::Update])` *before* `load_full_snapshot_with_peer_records`. Pass the resulting `Subscription` into `spawn_observer`.
- Adjust `spawn_observer` signature to accept an existing `Subscription` instead of opening one itself.
- Test: `subscribe_before_snapshot_drains_queued_events`.

### PR 4 — Observer rewrite

- Replace `spawn_observer`'s body with the debounced burst-coalescer described in §3.1.3.
- Wire the existing `selected_agent_did_rx` for drop-recovery scope.
- New file `client/observe/tests.rs` with the unit tests in §4.1.
- Benchmark scenario: 100 token updates in 150 ms against a 1 000-message session. Assert per-event latency under threshold.

### PR 5 — Materialization-supervisor scope

- Replace the `load_full_snapshot` call in `client/core/materialization.rs:120` with `load_agent_scoped_snapshot` keyed by the repair target's `agent_did`.
- Existing materialization tests must remain green.

### PR 6 — Refresh paths scope

- Replace `ClientCore::refresh_store` body with `load_agent_scoped_snapshot` if a selection is set; fall back to `load_full_snapshot` if not.
- Replace `ClientCore::refresh_remote_agent` body similarly (already keyed by agent_did).

### PR 7 — Lazy load on selection switch

- Add `ensure_agent_loaded(agent_did)` to `ClientCore` with a `Mutex<HashMap<String, Instant>>` debouncer (2 s).
- Call it from the selection-switch tauri command after updating `selected_agent_did`.
- Tests: `selection_switch_loads_new_scope_lazily`, `selection_switch_debounces_repeats`.

### PR 8 — Documentation and metrics

- Add the conformance paragraph in §4.3 to `crates/defra-agent/proofs/client-state-machine.md` (under §Subscription Model).
- Add `ObserverMetrics` (atomic counters) to `client/observe.rs`. Expose `ClientCore::observer_metrics()`.
- Add a diagnostics tauri command surfacing the metrics.

## 10. Acceptance criteria mapping

The original issue named three acceptance criteria. Mapped to this design:

1. *`load_full_snapshot()` is no longer called on every observed update.* — PR 4 removes the call from `spawn_observer`'s steady state. It survives only in bootstrap and as a no-selection drop-recovery fallback.
2. *Session history of 1 000+ messages doesn't cause observation lag.* — Satisfied for **steady-state observation** (per-event hot loop) by PR 4 and gated by the benchmark in §4.2 (`incremental_observer_handles_long_session`). Not yet satisfied for **initial bootstrap** of a long session — bootstrap still runs the full snapshot once. That last mile is a follow-up gated on Rust port of `defradb#4617`.
3. *Monotonicity proof in `Proofs/Client.lean` still holds.* — §4.3 establishes that the proof is silent on snapshot construction; the new conformance paragraph documents the invariant the design relies on.
