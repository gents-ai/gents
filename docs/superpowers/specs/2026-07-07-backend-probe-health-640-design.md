# Backend probe health (#640) — scheduled probing, hysteresis, truthful signal

**Issue:** #640 — inference backend probes report false-healthy for dead endpoints.
**Branch:** `fix/backend-probe-false-healthy-640` (worktree `../defra-agent-backend-probe-640`).

## Problem

`probe_and_promote_enabled_backends` (`backend_registry.rs:352`) is a startup-only,
promote-only ratchet: it skips backends already `healthy`, nothing ever writes
`unhealthy`, there is no schedule, and `last_probe` (which exists in the SDL) is
never written. `probe_status` is a stored constant after first promotion, and the
`defra_agent_backend_probe_status` metric faithfully reports that constant —
hence the fleet evidence (spark-1 dead 16h, 16/16 runtimes "healthy", metric
pinned at 1 through a 3h outage). `is_available()` gates admission and behavior
runnability on that constant, so the false signal feeds a real routing gate.

## Design decision: per-runtime, in-memory measured health

Reachability is observer-relative: runtime A reaching a backend that runtime B
cannot are both right. The deployment-routing model (each `(did, behavior)` on
exactly one deployment) means only the **local** runtime's reachability matters
for **its** routing. Approved shape (Jack, 2026-07-07):

- **Measured state lives in-memory only** — a `BackendHealthMap` per runtime,
  mirroring the MCP `ServiceHealthMap`. No new collection, no per-interval
  writes to fleet-replicated config docs, no conflicting-observer merges.
  Consequence accepted: measured state resets on restart (a dead backend is
  doc-available for up to K×interval after restart until probes demote it);
  the doc-sourced desktop view keeps showing operator intent, not measurement.
- **The shared `InferenceBackend` doc stays the operator-intent knob.**
  `probe_status` on the doc means "intended/bootstrap status"; the manual
  `config backend set --probe-status` override is untouched.
- **Recurring promote:** the scheduled prober also promotes the shared doc
  `unknown → healthy` on probe success (stamping doc `last_probe`), closing the
  adjacent gap where a backend dead at startup but recovering later stayed
  `unknown` forever. Idempotent, transition-only, all writers agree on the value.

## Components

### 1. Lean model — `Proofs/BackendHealth/` (spec leads)

Mirrors `Proofs/MCPHealth` structure with a simpler machine (no staleness, no
backoff, no removal):

- `State.lean`: `HealthState ::= unknown | healthy | degraded | unhealthy`;
  `Model = { state, failureCount }`; `blocksRouting s ↔ s = unhealthy`;
  `effectiveAvailable intent m ↔ intent ∧ ¬ blocksRouting m.state`.
- `Transition.lean`: events `probeSuccess | probeFail`; `step`:
  success → `{healthy, 0}`; fail → count+1, state = `count+1 ≥ K ? unhealthy : degraded`.
- `Properties.lean` (zero `sorry`): demotion at exactly K consecutive failures;
  no demotion below K (flap resistance); single-success promotion from any
  state; success resets the counter; intent=false is never overridden.
- `Executable.lean`: exhaustive `TransitionCase` enumeration over
  `K ∈ {1,2,3} × states × counts × events` (MCPHealth `TransitionCase.build`
  pattern), emitted as `backend_health_cases` in
  `Conformance/Contracts/Json/Snapshot.lean`, with `CoverageLedger.lean`
  consumer entries (ledger ids on BOTH sides).

No `ApplyReconcile` extension: no config-collection schema change (verified:
health state is runtime-owned, like `ToolServiceHealthState` which is also
outside the config-apply set).

### 2. Runtime — `crates/defra-agent/src/backend_health.rs`

- `BackendHealthMap` (public, like `ServiceHealthMap`): `backend_id →
  { state, failure_count, last_probe_at, last_error }`, with `snapshot()` /
  `get()` for consumers (#631's retry logic reads this handle).
- `BackendProberOptions { probe_interval: 60s, probe_timeout: 10s,
  failure_threshold_k: 3 }` on `DocumentRuntimeOptions` next to
  `health_checker_options`.
- `spawn_backend_prober(...)` in `run_agent` next to `spawn_health_checker`;
  a testable `run_backend_probe_cycle(backends, now, client, map, options)`
  core (the `run_health_check_cycle` pattern). Probes via
  `backend_provider::discover_models` (same call the startup ratchet uses).
  ChatGPT-Codex backends keep the existing skip (OAuthCredential is
  agent-scoped) → never probed → never demoted → doc-status governed.
- Every attempt stamps `last_probe_at` in the map. Transitions across the
  routing boundary (healthy↔unhealthy) log at `warn!` and nudge reconcile;
  steady-state failures log at `debug!` (no per-cycle spam).

### 3. Routing integration

- `run_control_watcher` gains a `health_events` receiver arm: a routing-relevant
  transition marks the view dirty and debounces into the normal resolve→propose
  path. Snapshot resolution consults the `BackendHealthMap` (handle plumbed via
  `DocumentResolveContext`).
- `BackendAdmissionConfig` gains `measured_unhealthy: bool`;
  `is_available() = enabled && probe_status=="healthy" && !measured_unhealthy`.
  Behavior availability (`document_view/snapshot.rs`) uses the same merge, so
  admission controllers, executor capacity, and behavior runnability all
  inherit the truthful signal. Demotion reaches routing within
  `probe_interval × K + debounce`.
- Startup resolution paths keep working: the map is empty at startup, so the
  merge is a no-op there (doc intent governs until the first probe cycle).

### 4. Truthful metric — `defra-agent-cli`

`serve` creates the `BackendHealthMap` before building the node, passes clones
into `DocumentRuntimeOptions` and `RuntimeHttpState` (same process). The
prometheus renderer overlays measured state:

- `defra_agent_backend_probe_status{backend_id, status=<measured>}` = **1 iff
  measured healthy, else 0** (unprobed/Codex backends fall back to doc status).
- `defra_agent_backend_last_probe_seconds` from the map's `last_probe_at`.

### 5. Acceptance tests (gate: `cargo test -p defra-agent`, full package)

- Conformance: generated Lean `backend_health_cases` vs the Rust step function
  (mirror `tests/conformance/mcp_health.rs`).
- Fleet-evidence repro: real local HTTP listener as backend → healthy; drop
  listener (connect refused) → after K cycles effective availability false,
  metric renders 0, `last_probe` fresh; restore listener → 1 success cycle →
  available again.
- Reconcile integration: health transition → proposal → admission registry
  drops/readmits the backend.
- Recurring promote: doc `unknown` + backend reachable → doc promoted to
  `healthy` with `last_probe` stamped, not just at startup.
- Operator override: `config backend set --probe-status` behavior unchanged.

## Out of scope

- #631 integration (we expose `BackendHealthMap` + note the interface on the
  issue; that session consumes it).
- Desktop surface for measured health (doc view keeps showing operator intent).
- Probing ChatGPT-Codex backends (agent-scoped OAuth constraint inherited).
- Persisted per-host health rows (revisit only if restart-reset or cross-host
  observability bites; `ToolServiceHealthState` is the template if so).
