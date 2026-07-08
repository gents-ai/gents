# Backend Probe Health (#640) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scheduled per-runtime backend probing with K=3 hysteresis feeding `is_available()`, a truthful `defra_agent_backend_probe_status` metric (can read 0), and fresh `last_probe`, per `docs/superpowers/specs/2026-07-07-backend-probe-health-640-design.md`.

**Architecture:** Lean model `Proofs/BackendHealth` (spec leads) → generated `backend_health_cases` conformance fence → in-memory `BackendHealthMap` written by a scheduled prober in `run_agent`, merged into `BackendAdmissionConfig`/behavior availability at snapshot resolution, nudging the control watcher on routing-relevant transitions; the CLI `serve` process shares the map handle with the HTTP metrics renderer.

**Tech Stack:** Lean 4 (lake, mathlib cache via parent-worktree symlink), Rust (tokio, reqwest, axum), DefraDB GraphQL.

## Global Constraints

- Lean first, zero `sorry`s; conformance ledger ids updated on BOTH sides (CoverageLedger.lean + tests/conformance/coverage ledger consumers).
- Gate with `cargo test -p defra-agent` (full package, NOT `--lib`); CLI changes also `cargo test -p defra-agent-cli`.
- `tracing`, never `println`; probe-failure logs must not spam (warn on transition only).
- Always `graphql::escape_graphql_string()` for interpolated GraphQL; never emit `[]` literals in mutations.
- Defaults: probe_interval=60s, probe_timeout=10s, failure_threshold_k=3; 1 success promotes.
- ChatGPT-Codex backends are never probed and never demoted (agent-scoped OAuthCredential).
- Fresh-worktree Lean: symlink parent's `.lake/packages/mathlib/.lake/build` (do NOT `lake exe cache get`).

---

### Task 1: Lean model — Proofs/BackendHealth (State, Transition, Properties)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/BackendHealth/State.lean`
- Create: `crates/defra-agent/proofs/Proofs/BackendHealth/Transition.lean`
- Create: `crates/defra-agent/proofs/Proofs/BackendHealth/Properties.lean`
- Create: `crates/defra-agent/proofs/Proofs/BackendHealth.lean` (imports the three)
- Modify: proofs root import list (wherever siblings like `Proofs.MCPHealth` are imported — check `crates/defra-agent/proofs/Proofs.lean` / lakefile globs)

**Interfaces:**
- Produces (consumed by Task 2 + Rust step function):
  - `HealthState ::= unknown | healthy | degraded | unhealthy` with `toDefraDB`
  - `Event ::= probeSuccess | probeFail` with `toDefraDB`
  - `Model = { state : HealthState, failureCount : Nat }`, `Model.initial = { state := .unknown, failureCount := 0 }`
  - `step (m : Model) (e : Event) (K : Threshold) : Model` (total — no removal event; reuse `Threshold` shape `{ k : Nat // k ≥ 1 }`)
  - `blocksRouting : HealthState → Bool` (true iff `.unhealthy`)
  - `effectiveAvailable (intent : Bool) (m : Model) : Bool := intent && !(blocksRouting m.state)`

Transition semantics (mirror MCPHealth `step?` minus staleness/backoff/removal):

```lean
def step (m : Model) (e : Event) (K : Threshold) : Model :=
  match e with
  | .probeSuccess => { state := .healthy, failureCount := 0 }
  | .probeFail =>
      let n := m.failureCount + 1
      if n ≥ K.val then { state := .unhealthy, failureCount := n }
      else { state := .degraded, failureCount := n }
```

Properties to prove (zero sorry), with `run` = foldl of `step`:
- `demotes_at_K`: from any `m` with `failureCount = 0`, `run m (List.replicate K.val .probeFail) K` has state `.unhealthy`.
- `no_demote_below_K`: for `n < K.val`, `run m (List.replicate n .probeFail) K` (from `failureCount = 0`) never `.unhealthy`.
- `single_success_promotes`: `(step m .probeSuccess K).state = .healthy` and count 0, for all `m`.
- `success_resets_count`: corollary, stated explicitly.
- `intent_never_overridden`: `effectiveAvailable false m = false` for all `m`.
- `unknown_does_not_block`: `blocksRouting .unknown = false` (startup grace).

**Steps:**
- [ ] Symlink mathlib build cache from parent worktree; `cd crates/defra-agent/proofs && lake build Proofs.BackendHealth` fails (module absent) — then create the four files.
- [ ] `lake build` — expect success, zero sorry.
- [ ] Commit: `spec(lean): BackendHealth model — K-failure demotion, single-success promotion (#640)`

### Task 2: Lean executable cases + snapshot emission + ledger

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/BackendHealth/Executable.lean` (mirror `MCPHealth/Executable.lean`: `TransitionCase.build` over `K ∈ {1,2,3} × HealthState.all × counts 0..K × Event.all`, `transitionCases : List TransitionCase`)
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/ToolExecution.lean` (or a sibling Json module) — add `backendHealthCaseJson` mirroring `mcpHealthCaseJson` (fields: name, start_state, start_count, event, threshold_k, next_state, next_count; NO rust_projection/no null next — step is total, so emit plain strings)
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean` — add `"backend_health_cases": jsonArray (Proofs.BackendHealth.transitionCases.map backendHealthCaseJson) ++ ","`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` — consumer entries:
  - `consumerCoverage "backend_health_cases" "BackendHealthCases" "backend_health::tests::generated_backend_health_cases_match_prober_transitions"` tagged `"backend-health" [Surface.runtimeInternal]`
  - conformance-binary shape fence: `"conformance::backend_health_cases_pin_threshold_shape"` (exact id decided by structure fence conventions in `tests/conformance/coverage.rs` — keep both sides in sync)

**Steps:**
- [ ] `lake build && lake exe <contract generator>` (same command `defra-agent-lean-contract::run_contract_generator` uses — see that crate for the exe name); verify emitted JSON contains `backend_health_cases`.
- [ ] Commit: `spec(lean): emit backend_health_cases + ledger consumers (#640)`

### Task 3: Rust conformance fences (red → green with Task 4)

**Files:**
- Modify: `crates/defra-agent/src/lean_vocab_test.rs` + new `crates/defra-agent/src/lean_vocab_test/backend_health.rs` — `LeanBackendHealthCase { name, start_state, start_count, event, threshold_k, next_state, next_count }` (serde, mirror `LeanMcpHealthCase` in `slot_persistence_health.rs:129` minus Option-ness), snapshot field `backend_health_cases`, accessor `lean_backend_health_cases()`.
- Create: `crates/defra-agent/tests/conformance/backend_health.rs` — shape fence mirroring `tests/conformance/mcp_health.rs` (non-empty, K=1..3 present, `next_state == "unhealthy"` only via `probeFail`, `probeSuccess` always lands `healthy`/count 0). Register in `tests/conformance/structure.rs` home table + `coverage.rs` ledger consumer list.

**Steps:**
- [ ] Write fences; `cargo test -p defra-agent --test conformance backend_health` — PASS (cases emitted by Task 2); the `backend_health::tests` consumer id stays red until Task 4 exists (ledger coverage test enforces).
- [ ] Commit: `test(conformance): backend_health_cases fences (#640)`

### Task 4: `backend_health.rs` — map, state machine, prober cycle

**Files:**
- Create: `crates/defra-agent/src/backend_health.rs`
- Modify: `crates/defra-agent/src/lib.rs` — `pub mod backend_health; pub use backend_health::{BackendHealthMap, BackendHealthSnapshot, BackendProberOptions, spawn_backend_prober, run_backend_probe_cycle};`

**Interfaces (produced):**

```rust
#[derive(Clone, Debug)]
pub struct BackendProberOptions {
    pub probe_interval: Duration,   // 60s
    pub probe_timeout: Duration,    // 10s
    pub failure_threshold_k: u32,   // 3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendHealthState { Unknown, Healthy, Degraded, Unhealthy }
impl BackendHealthState { pub fn blocks_routing(self) -> bool; pub fn as_str(self) -> &'static str; }

#[derive(Debug, Clone)]
pub struct BackendHealthSnapshot {
    pub backend_id: String,
    pub state: BackendHealthState,
    pub failure_count: u32,
    pub last_probe_at: DateTime<Utc>,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
pub struct BackendHealthMap { /* Arc<RwLock<HashMap<String, Entry>>> */ }
impl BackendHealthMap {
    pub fn new() -> Self;
    pub async fn get(&self, backend_id: &str) -> Option<BackendHealthSnapshot>;
    pub async fn snapshot(&self) -> HashMap<String, BackendHealthSnapshot>;
    pub async fn measured_blocks_routing(&self, backend_id: &str) -> bool; // false when absent/unknown
}

fn step_backend(prev: (BackendHealthState, u32), event: ProbeEvent, k: u32) -> (BackendHealthState, u32); // mirrors Lean step
pub async fn run_backend_probe_cycle(
    node: &EmbeddedNode, client: &reqwest::Client, now: DateTime<Utc>,
    health_map: &BackendHealthMap, options: &BackendProberOptions,
) -> Vec<String>; // backend_ids whose blocks_routing changed this cycle
pub fn spawn_backend_prober(
    node: Arc<EmbeddedNode>, health_map: BackendHealthMap,
    options: BackendProberOptions, health_events_tx: mpsc::Sender<()>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()>;
```

Cycle behavior:
- `list_enabled_backends`, skip `BackendProviderKind::ChatGptCodex` (existing constraint comment), probe via `crate::backend_provider::discover_models` under `tokio::time::timeout(options.probe_timeout, ..)`.
- Step the machine; stamp `last_probe_at = now` on EVERY attempt; retain only currently-enabled backends (drop rows for deleted/disabled).
- On probe success where the doc's `probe_status == "unknown"`: `set_backend_probe_status(node, id, "healthy")` AND stamp doc `last_probe` (extend `set_backend_probe_status` with an optional timestamp or add `promote_backend_with_last_probe`). Recurring-promote decision.
- Transition logging: `warn!` only when `blocks_routing` flips (either direction), `debug!` otherwise. Loop mirrors `spawn_health_checker` (interval + MissedTickBehavior::Skip + cancel).
- After a cycle with any flipped backend: `let _ = health_events_tx.try_send(());`.

**Steps:**
- [ ] Write `generated_backend_health_cases_match_prober_transitions` test in `backend_health.rs::tests` driving `step_backend` over `lean_backend_health_cases()` (mirror `health_checker.rs:1078`). Run: FAIL (module absent).
- [ ] Implement; `cargo test -p defra-agent --lib backend_health` PASS.
- [ ] Cycle test with real listeners: bind `TcpListener` + minimal axum `/v1/models` responder → healthy; drop server (connect refused) → 3 cycles → `Unhealthy`, `measured_blocks_routing` true, `last_probe_at` advances every cycle; rebind → 1 cycle → `Healthy`. Also: codex-kind backend never probed/demoted; doc `unknown` promoted to `healthy` on success (fake with embedded node helper like `tests/support`).
- [ ] Commit: `feat: scheduled backend prober with K-failure hysteresis (#640)`

### Task 5: Merge measured health into availability + control-watcher nudge

**Files:**
- Modify: `crates/defra-agent/src/admission/config.rs` — `BackendAdmissionConfig { ..., pub measured_unhealthy: bool }`; `is_available()` gains `&& !self.measured_unhealthy`; `from_backend(backend, measured_unhealthy: bool)`; `backend_admission_configs_from_backends(backends, health: &BackendHealthMap)` becomes async (or takes a pre-fetched `HashMap<String, bool>` — prefer the sync map arg to avoid async contagion: callers snapshot the health map first).
- Modify: `crates/defra-agent/src/agent/document_view/snapshot.rs:113` (behavior availability: unavailable reason `"backend <id> is measured unhealthy by the local prober"`) and `:291` (configs) to pass the measured map.
- Modify: `crates/defra-agent/src/agent.rs` — `DocumentResolveContext { ..., backend_health: BackendHealthMap }`; `DocumentRuntimeOptions { ..., backend_prober_options: BackendProberOptions, backend_health: Option<BackendHealthMap> }` (None → runtime creates one); `DefraAgent::backend_health()` accessor (the #631 signal).
- Modify: `crates/defra-agent/src/agent/runtime/startup.rs` — spawn prober next to `spawn_health_checker` (line ~91) with a `mpsc::channel(1)`; legacy startup config path (`startup.rs:656` + `builder.rs:465,525`) passes an empty measured map (no-op at startup by design).
- Modify: `crates/defra-agent/src/agent/runtime/control_watcher.rs` — new param `health_events_rx: mpsc::Receiver<()>`; select arm: `Some(()) = health_events_rx.recv() => { dirty = true; sleep.reset(now + CONTROL_RECONCILE_DEBOUNCE); }` (no settle window — local signal, nothing to materialize).
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs:462` — thread measured map if the shim has agent handle access; otherwise leave doc-only and note in PR (verify reachability during implementation).

**Steps:**
- [ ] TDD: runtime test (pattern: `agent/runtime/tests/startup_recovery.rs:302`) — behavior on measured-unhealthy backend becomes unavailable within one proposal round after the health map flips + event fires; recovers on flip back. Run FAIL → implement → PASS.
- [ ] `cargo test -p defra-agent` full package green.
- [ ] Commit: `feat: measured backend health gates admission and behavior availability (#640)`

### Task 6: CLI — serve wiring + truthful metric

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/serve.rs` — create `BackendHealthMap` before node build; pass into `DocumentRuntimeOptions` and `runtime_contract_router`.
- Modify: `crates/defra-agent-cli/src/http/router.rs` — `RuntimeHttpState { ..., backend_health: Option<defra_agent::BackendHealthMap> }` (router fn gains the param; other constructors pass None).
- Modify: `crates/defra-agent-cli/src/http/prometheus.rs` — `render_prometheus_metrics` takes the optional measured snapshot; per backend: `status` label = measured state string when present else doc `probe_status`; sample value = `1` iff that resolved status is `"healthy"` else `0`; `defra_agent_backend_last_probe_seconds` prefers measured `last_probe_at` (fall back to doc `last_probe`).
- Modify: `crates/defra-agent-cli/src/http/healthz.rs` — include measured state per backend in payload if trivially threadable (same data source), else skip.

**Steps:**
- [ ] TDD on the render fn with fixture data: dead measured backend renders `defra_agent_backend_probe_status{backend_id="b",status="unhealthy"} 0` and fresh `last_probe_seconds`; absent measured falls back to doc status with value 1 for healthy. FAIL → implement → PASS.
- [ ] `cargo test -p defra-agent-cli` green; manual override test (`config backend set --probe-status`) untouched/green.
- [ ] Commit: `feat(cli): backend probe metric reflects measured health, can read 0 (#640)`

### Task 7: Docs, proofs README, issue notes, final gate

**Files:**
- Modify: `crates/defra-agent/proofs/README.md` — add BackendHealth to the proven-core map.
- Modify: `docs/backends.md` — probe lifecycle section (scheduled probing, hysteresis, operator override semantics, metric meaning).
- Comment on #640 + #631 (interface note: `DefraAgent::backend_health()` / `BackendHealthMap`).

**Steps:**
- [ ] Full gate: `cargo test -p defra-agent && cargo test -p defra-agent-cli` + `lake build` + conformance suite; fix stragglers (desktop compile if `InferenceBackend` struct changed — it didn't gain fields, so none expected).
- [ ] `cargo fmt --all && cargo clippy -p defra-agent -p defra-agent-cli` clean.
- [ ] Commit remaining docs; push branch; PR referencing #640 with fleet-evidence repro description.

## Self-review notes

- Spec coverage: scheduled probe + last_probe (T4), hysteresis (T1/T4), truthful metric (T6), auth-shape skip (T4), #631 signal (T5 accessor), fleet-evidence repro test (T4/T5), restart survival = prober spawns in run_agent (T5), override unchanged (T6 test), log rate-limit = transition-only warn (T4).
- Types consistent: `BackendHealthMap`/`BackendHealthSnapshot`/`BackendProberOptions` named identically across T4–T6; `measured_unhealthy` field name fixed in T5.
- No placeholders: Lean `step` given verbatim; Rust interfaces given as signatures with semantics enumerated; remaining code mirrors named anchors (`MCPHealth/Executable.lean`, `health_checker.rs` loop, `mcp_health.rs` fence) which the executor reads directly.
