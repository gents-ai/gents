# MCPHealth: K≥2 backoff behavior (design)

Status: design for review
Date: 2026-05-20
Tracking: issue #253; audit item #2 Stage 2 in
`docs/superpowers/audits/2026-05-19-conformance-audit.md` §10 and §6
Branch: `design/issue-253-mcp-health-k2-design`
Predecessor: PR #257 (Stage 1, K=1 runtime drive — landed)

## Goal

Land the Rust K≥2 MCPHealth/backoff behavior the Lean spec already shapes,
consume the full emitted `mcp_health_cases` domain (drop the
`lean_mcp_health_k1_cases()` filter), and promote the
`mcp_health_cases` ledger row from `consumerWithFollowUpCoverage` to
`consumerCoverage`. This spec does not write code; it commits to the
shape of the K≥2 driver and the smallest delta to get there.

## Source of Truth

- `crates/defra-agent/proofs/Proofs/MCPHealth/{State,Transition,Properties,Coupling,Executable}.lean`.
- `docs/superpowers/audits/2026-05-19-conformance-audit.md` §10 (MCPHealth)
  and §6 item #2 (recommended next-impl order).
- `crates/defra-agent/src/health_checker.rs` — current K=1 driver and Stage-1
  conformance consumer.
- `crates/defra-agent/src/mcp_pool.rs` — pool integration (`list_tools` /
  `remove`).
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:611` —
  current ledger row with the explicit #253 follow-up text.
- `docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md` — original
  Lean-side design.

## Current State

### What Lean models today

`HealthState` is the four-state lifecycle (`healthy`, `degraded`, `evicted`,
`reconnecting`) at `Proofs/MCPHealth/State.lean:26`; `ServiceModel` carries
state + `failureCount` at `:67`. Events are
`probeSuccess(staleness)`, `probeFail`, `backoffExpiry`, `registryAbsent`
at `:79`.

`step?` at `Proofs/MCPHealth/Transition.lean:37-49` is K-parameterized
(`Threshold := { k : Nat // k ≥ 1 }`). The K=1 collapse is witnessed by
`h7_k1_collapse_probefail_skips_degraded` at `Properties.lean:64`. H5 —
`h5_anti_flapping_inter_eviction_gap` at `Properties.lean:307` — proves
that between any two evictions reached from a healthy prefix the slice
contains at least `K` `probeFail` events. H6
(`[backoffExpiry, probeSuccess false]` → healthy) and H6'
(`probeSuccess false` directly → healthy) at `Properties.lean:94`/`:106`
together establish that `Reconnecting` is an **optional** pass-through, not
a mandatory recovery state.

Conformance rows are emitted by
`Proofs/MCPHealth/Executable.lean:73` enumerating
`K ∈ {1, 2, 3} × HealthState × startCount ∈ [0..K) × Event`. The full row
list is `transitionCases`; `k1ProjectionCases` filters K=1 only and is
what Rust currently consumes. The 3-state `rustProjection` collapses
`evicted`/`reconnecting` → `unreachable`.

The Lean spec is **shape-complete for K≥2 thresholding** (count semantics,
state graph, projection). **The backoff schedule (duration of
`backoffExpiry`) is intentionally not modeled** — `backoffExpiry` is an
opaque event without a duration parameter, and no Properties theorem
prescribes a cadence. Schedule is therefore a Rust-side operational
knob, not a Lean delta.

### What the current Rust K=1 path does

`spawn_health_checker` at `health_checker.rs:109` runs a 30s `tokio::time::interval`
loop that calls `run_health_check` → `run_health_check_cycle`. Per service:

- Query `ToolServiceRegistry` for online rows.
- Probe with a 5s timeout via `mcp_pool.list_tools(&service_id, &endpoint)`
  at `:269`.
- On success: mark `Healthy` (or `Stale` if heartbeat age > 120s).
- On failure or timeout: `mcp_pool.remove(&service_id)` and mark
  `Unreachable`.

`ServiceHealthMap` entries carry `{status: HealthStatus, last_seen, last_error}`
only. There is **no failure counter, no backoff timer, no `Reconnecting`
state**. K=1 collapses every `probeFail` to immediate eviction by H7.

`mcp_pool.remove(&service_id)` drops a cached connection; it is **not** an
admission gate. The next `call_tool` lazy-reconnects via
`get_or_connect`. (`mcp_pool.rs:213` for `list_tools`; the audit's
`Coupling.lean` C1–C4 — Evicted/Reconnecting must block dispatch as
`ServiceUnavailable` via `preflight` — is unwired today and is **out of
scope** for #253.)

The Stage-1 conformance consumer is
`generated_mcp_health_k1_cases_match_health_checker_transitions` at
`health_checker.rs:508`. It calls `run_health_check_cycle` directly,
seeding `ServiceHealthMap` with the row's `start_state` before each call,
filtering the Lean rows via `lean_mcp_health_k1_cases()` at
`lean_vocab_test.rs:511`, and short-circuiting the `backoffExpiry` event by
returning `start_status` unchanged (`health_checker.rs:457`) — fine at
K=1 because no backoff is armed.

The ledger row at
`Proofs/Conformance/CoverageLedger.lean:611` is already
`consumerWithFollowUpCoverage` with the explicit text:

> "Issue #253 adds K≥2 MCPHealth backoff behavior in Rust and drops the
> `lean_mcp_health_k1_cases()` filter so the full emitted `mcp_health_cases`
> domain is consumed."

### What's missing for K≥2

Three things the K=1 path cannot represent:

1. **A per-service failure counter** (Lean's `failureCount`). Without it,
   the runtime cannot distinguish "first probeFail" (Lean: `degraded,
   failureCount=1` at K≥2) from "Kth probeFail" (Lean: `evicted,
   failureCount=K`).
2. **A backoff window** (Lean's `backoffExpiry`). Once `failureCount ≥ K`
   the runtime must skip probes until the backoff expires. Today every
   tick probes unconditionally.
3. **A way for the conformance test to drive `backoffExpiry` and
   `probeFail`-with-non-zero-`startCount` rows.** Stage 1 short-circuits
   `backoffExpiry` because K=1 has no observable behavior; at K≥2 the
   runtime must produce a real post-state per row.

## Design Choices

### Choice 1 — Backoff schedule: exponential capped, no jitter

The Lean spec is schedule-agnostic, so the choice is purely operational.

Options considered:

- **(A) Linear / fixed**: always wait the cycle interval (30s) after the
  Kth failure. Simplest. Equivalent to "probe every tick after the
  threshold." No reconnect-storm protection if a flaky service comes
  back: every deployment with a stuck call hits it at the same tick.
- **(B) Exponential capped** (recommended): wait `min(base × 2^attempts,
  cap)` after eviction, where `attempts` is the number of consecutive
  full-backoff cycles. Concrete: `base = 30s`, doubling sequence
  `30s → 60s → 120s → 240s → 480s`, cap at `600s` (10 min). Deterministic
  for tests; integer arithmetic; gives a fast first retry and reasonable
  back-pressure on persistent outages.
- **(C) Exponential + full jitter**: random in `[0, current_cap]`. AWS's
  recommended default for high-fan-in client retries. In a
  single-deployment context (one health checker per node) the
  thundering-herd risk it solves is marginal, and it adds non-determinism
  to the conformance test surface.

**Recommendation: (B).** Single-deployment context, no cross-replica
coordination required (see Memory entry: `deployment_routing_model`),
deterministic tests, prevents reconnect storms when a service returns
after a long outage. The schedule lives behind a `backoff_duration(attempts)`
helper so a later swap to jitter is one function.

The K=1 case is preserved as a special case: when `K = 1`, `failureCount`
never lands strictly between 0 and K, so there is no "degraded by failure
count" flavor. The backoff schedule still applies — at K=1 a single
probeFail evicts AND arms the backoff. Today's K=1 runtime does *not*
arm a backoff; this is a behavior change at K=1, but the Lean rows do
not constrain it (the K=1 `backoffExpiry` row says `evicted_0 →
reconnecting_0`, and our chosen H6'-path runtime collapses that into
Unreachable → Healthy on the next successful probe). See Choice 4.

### Choice 2 — K placement: agent-config-configurable, default K=3

Options considered:

- **(A) Hardcoded constant**: `const FAILURE_THRESHOLD_K: u32 = 3;` in
  `health_checker.rs`. Simplest. Mirrors today's `HEALTH_CHECK_INTERVAL` /
  `STALENESS_THRESHOLD_SECS` / `PROBE_TIMEOUT` constants.
- **(B) Agent-config-configurable** (selected): introduce a `HealthCheckerOptions`
  struct with `#[derive(Default)]`, threaded through `DefraAgent` like
  `retry_policy` / `hook_failure_policy` (`crates/defra-agent/src/agent.rs:78`),
  so operators can override K (and the rest of the backoff schedule) at
  startup without recompile.
- **(C) Per-service via `ToolServiceRegistry`**: each registry row declares
  its own K. Heaviest; not justified by today's use cases.

**Selected: (B).** Concrete shape:

```rust
#[derive(Clone, Debug)]
pub struct HealthCheckerOptions {
    pub cycle_interval: Duration,
    pub probe_timeout: Duration,
    pub staleness_threshold: Duration,
    pub failure_threshold_k: u32,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
}

impl Default for HealthCheckerOptions {
    fn default() -> Self {
        Self {
            cycle_interval: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(5),
            staleness_threshold: Duration::from_secs(120),
            failure_threshold_k: 3,
            backoff_initial: Duration::from_secs(30),
            backoff_max: Duration::from_secs(600),
        }
    }
}
```

`HealthCheckerOptions` is added to `DocumentRuntimeOptions`
(`crates/defra-agent/src/agent.rs:73`) and the builder. `spawn_health_checker`
takes it as a final argument; the call site at
`agent/runtime/startup.rs:77` passes the resolved value.

Default keeps today's `cycle_interval` / `probe_timeout` /
`staleness_threshold` numbers unchanged. The five module-level `const`s in
`health_checker.rs` become struct defaults. Existing tests that import
`STALENESS_THRESHOLD_SECS` (`health_checker.rs:399`) move to importing
`HealthCheckerOptions::default().staleness_threshold`.

K=3 by default matches the "small but not paranoid" target the
audit's smallest-delta paragraph implies — two transient blips do not
evict; three do. Operators can drop to K=1 to recover today's behavior, or
raise on links known to flap.

### Choice 3 — State persistence: memory-only, restart re-derives

Options considered:

- **(A) Memory-only** (selected): extend `ServiceHealthMap`'s value type
  with the new internal fields. On daemon restart, the map is empty and
  every service starts at `failure_count = 0` / no backoff. The first
  cycle re-probes everything, which is the right behavior: at restart we
  don't know what's actually true; re-probing is fast (≤ probe timeout
  per service) and gives a fresh, accurate read.
- **(B) Persisted to DefraDB**: write `{failure_count, backoff_until}` to
  a new `ServiceHealthState` collection (or extend `ToolServiceRegistry`).
  Survives restarts. But: `ToolServiceRegistry` is gossiped (P2P), and
  health is **per-deployment** — each node has its own connectivity view.
  Persisting it shared would be incorrect; persisting it local-only adds
  a collection that buys nothing the in-memory map can't.

**Selected: (A).** Per-deployment routing (see memory:
`deployment_routing_model`) means each node's MCP-health view is local
truth. Restart-on-failure is the only way K-state could go stale; in
that window the right answer is to re-probe, not to replay stale
counters. Avoids a new DefraDB collection and avoids the gossip-vs-local
correctness trap.

### Choice 4 — `Reconnecting` visibility: skip (H6' path)

The Lean spec lets `Reconnecting` exist (H6: two-step recovery via
`[backoffExpiry, probeSuccess false]`) or be skipped (H6': single
`probeSuccess false` from `evicted` reaches `healthy` directly).

Options considered:

- **(A) Skip — H6' path** (selected): the runtime tracks `backoff_until`
  internally. When a cycle observes `now >= backoff_until` for an
  evicted service, it probes immediately. On success → `Healthy`; on
  failure → re-arm backoff with `attempts += 1` and stay `Unreachable`.
  `HealthStatus` stays 3 variants (`Healthy` / `Stale` / `Unreachable`).
- **(B) Model as transient observable state**: introduce a 4th
  `HealthStatus::Reconnecting` (projects to `unreachable` for callers).
  When backoff fires, flip `Evicted` → `Reconnecting` *before* probing;
  next probe transitions to `Healthy` / `Stale` / back to `Evicted`.
  Matches Lean rows exactly.

**Selected: (A).** Both Lean states project to `unreachable` in the
3-state `rustProjection`, so conformance is satisfied either way: the
Lean row `mcp_health_K2_evicted_K_backoffExpiry_reconnecting_K` has
`rust_projection = "unreachable"`, and so does the post-state in (A).
H6' is the simpler runtime; adding a 4th visible state buys nothing
unless preflight (Coupling C1–C4) is wired, which is a separate spec.

Internal bookkeeping for the test driver (Choice 5) preserves the Lean
distinction even though the projected `HealthStatus` does not: the
`step_service` helper takes/returns the Lean four-state directly.

### Choice 5 — Pool admission during backoff: no gate (out of scope)

The Lean `Coupling.lean` C1–C4 theorems state that Evicted and
Reconnecting must `block` dispatch as `ServiceUnavailable` via
`ToolExecution.preflight`. Today no `preflight` call exists in
`mcp_pool.call_tool`. Wiring preflight is the Coupling-side gap the audit
calls out separately and is **out of scope** for #253.

The runtime contract this design lands: during backoff,
`ServiceHealthMap` reports `Unreachable` (advisory), and the
health-checker itself does not issue probes. Tool callers that read the
map can choose not to dispatch; callers that ignore it will lazy-reconnect
through `mcp_pool` (today's behavior — unchanged).

## Smallest Delta

The K≥2 work is one Rust refactor plus one ledger row.

### Rust refactor

1. **Extend `ServiceHealthMap`'s value type.** Add
   `failure_count: u32` and `backoff_until: Option<DateTime<Utc>>` as
   internal-only fields. `ServiceHealth` (the public, advisory shape)
   keeps `{status, last_seen, last_error}` unchanged.

2. **Pull per-service decision into a pure helper.** New private
   function in `health_checker.rs`:

   ```rust
   fn step_service(
       prev: ServiceModelInternal,
       event: ProbeEvent,
       now: DateTime<Utc>,
       opts: &HealthCheckerOptions,
   ) -> ServiceModelInternal;
   ```

   where `ServiceModelInternal` carries the Lean 4-state plus
   `failure_count` and `backoff_until`, and `ProbeEvent` is the Lean
   `Event` vocabulary (`ProbeSuccess { stale: bool }`, `ProbeFail`,
   `BackoffExpiry`, `RegistryAbsent`). The helper implements Lean
   `step?` exactly — including `BackoffExpiry: Evicted → Reconnecting`.
   The H6'-path choice from Choice 4 is **a runtime-cycle decision, not
   a helper decision**: in production the cycle never dispatches
   `BackoffExpiry` to the helper; when backoff expires it dispatches
   `ProbeSuccess` (or `ProbeFail`) directly, taking `Evicted → Healthy`
   per H6'. The conformance test driver may dispatch `BackoffExpiry`
   directly to exercise the Lean `reconnecting` rows; the runtime never
   does.

3. **`run_health_check_cycle` calls `step_service`.** Per service, the
   cycle:

   - If the registry omits the service → `step_service(_, RegistryAbsent,
     _, _)` → drop the map entry (mirrors Lean `step? = none`).
   - Else if `failure_count >= K` and `now < backoff_until` → no event;
     leave the entry as-is. This is the "probe is suppressed during
     backoff" rule; no Lean row corresponds to a no-op tick.
   - Else if `failure_count >= K` and `now >= backoff_until` → probe.
     This is the H6' path: a single `probeSuccess` from `Evicted`
     reaches `Healthy` without an intermediate `Reconnecting`. On probe
     success → `step_service(prev, ProbeSuccess { stale }, now, opts)`.
     On probe failure → `step_service(prev, ProbeFail, now, opts)`
     re-arms backoff with `attempts += 1`.
   - Else (normal probe path) → probe; dispatch to `step_service` with
     `ProbeSuccess` or `ProbeFail`.

4. **Backoff helper:**

   ```rust
   fn backoff_duration(attempts: u32, opts: &HealthCheckerOptions) -> Duration {
       let scaled = opts.backoff_initial
           .saturating_mul(1u32.checked_shl(attempts).unwrap_or(u32::MAX));
       scaled.min(opts.backoff_max)
   }
   ```

   `attempts` counts consecutive eviction cycles (resets to 0 on any
   `probeSuccess`).

5. **Conformance consumer.** Rename
   `generated_mcp_health_k1_cases_match_health_checker_transitions` →
   `generated_mcp_health_cases_match_health_checker_transitions` and
   restructure to:

   - Iterate `lean_mcp_health_cases()` (drop the `_k1` filter).
   - Seed `ServiceModelInternal` from the row's `start_state` /
     `start_count` / `threshold_k`. For rows where `start_count` is
     inconsistent with `start_state` under the global invariant (e.g.,
     `evicted_0` at K≥2 — failureCount=0 can't actually be evicted in a
     reachable trace), set the fields exactly as the row says: the test
     is a per-step transition contract, not a global-invariant contract.
   - Apply the event by calling `step_service` directly with the row's
     `threshold_k` (override `opts.failure_threshold_k` for the call).
   - Compare the returned `ServiceModelInternal.state` projected through
     `healthProjection` to the row's `rust_projection`. Because the
     helper implements Lean `step?` exactly, `BackoffExpiry`-from-Evicted
     rows land in `Reconnecting` and project to `unreachable`, matching
     Lean directly.
   - Assert `failure_count` matches the row's `next_count` where set.

   Driving `step_service` directly (rather than `run_health_check_cycle`)
   is the right call here because per-row conformance is a transition
   contract: the cycle's job is to **decide which event to apply this
   tick**; the helper's job is to **apply the event**. Stage 1 routed
   through the cycle because K=1 had no per-service state to seed; at
   K≥2 the cycle wrapper adds noise without coverage. A separate
   integration test (one, not one per row) drives `run_health_check_cycle`
   through a multi-cycle sequence to fence the cycle's event-decision
   logic.

6. **Drop the filter.** Delete `lean_mcp_health_k1_cases()` from
   `lean_vocab_test.rs:511` once nothing references it.

### Conformance ledger consequence

`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:611` —
flip from `consumerWithFollowUpCoverage` to `consumerCoverage`, drop the
follow-up text, and update the consumer name from
`generated_mcp_health_k1_cases_...` to
`generated_mcp_health_cases_match_health_checker_transitions`. The two
lines of context comment above the row (`-- 2026-05-19 conformance audit
section 10 / section 6 item #2 ...`) can be replaced with a one-line
"Closed by #253" note.

No Lean delta. `Proofs/MCPHealth/{State,Transition,Properties,Coupling,Executable}.lean`
are untouched.

## Test Surface

The K≥2 driver tests in **two** layers:

### Layer 1 — per-row transition conformance

`generated_mcp_health_cases_match_health_checker_transitions` drives
`step_service` directly per Lean row. Pure, fast, deterministic. This is
where K≥2 coverage lives. No DB, no MCP pool, no synthetic clock —
`step_service` is a pure function of `(prev, event, now, opts)` and
`now` is passed in as a fixture timestamp.

### Layer 2 — cycle-level integration

One new test in `tests/state_machine_conformance/` or
`health_checker::tests` that drives `run_health_check_cycle` across a
multi-cycle sequence with synthetic time (`now: DateTime<Utc>` passed in
explicitly, as the cycle already accepts) and the existing
`McpPool::new_with_list_tools_handler` test seam. Verifies the cycle's
**event-decision logic**:

- A service that fails K consecutive cycles transitions
  `Healthy → Stale (×K−1) → Unreachable` and arms backoff.
- During backoff, no probe is issued (the mock pool handler is not
  called).
- After `backoff_until`, the next cycle probes; on success the service
  returns to `Healthy`; on failure attempts increments and the backoff
  cap applies.
- H5 witness: in any cycle sequence the number of probeFails between two
  evictions is ≥ K.

This is one test, not one per row, because the per-row contract is
already covered by Layer 1. Layer 2 is the cycle wrapper.

Neither layer needs a real MCP service; the existing test seam
(`McpPool::new_with_list_tools_handler` at `mcp_pool.rs:181`) is
sufficient.

## Design Questions — Answered

- **Backoff schedule.** Lean is schedule-agnostic (`backoffExpiry` carries
  no duration; no Properties theorem prescribes cadence). We choose
  exponential capped — see Choice 1.
- **State persistence.** Memory-only with restart re-probing — see
  Choice 3. Each deployment has its own connectivity view; no gossip;
  no new collection.
- **Pool admission interaction.** No gate. `mcp_pool.remove` continues to
  drop cached connections; the health checker simply does not probe
  during backoff. Wiring `preflight` (Coupling C1–C4) is a separate spec.
- **Test surface.** Two-layer: per-row transition conformance against
  `step_service` (no DB, no service mock); cycle-level integration uses
  the existing `McpPool` test seam.

## Risks + Open Questions

- **`failure_count` reset semantics on `RegistryAbsent`.** The Lean
  `step?` returns `none` for `RegistryAbsent` — the service is removed
  from the model. The runtime drops the map entry. If the service
  re-registers later, it re-enters at `failure_count = 0`. This is the
  intended behavior (operator removed the service deliberately) and
  matches Lean.

- **Behavior change at K=1.** Today's K=1 runtime does **not** arm a
  backoff (the audit's Stage 1 short-circuits `backoffExpiry` for this
  reason). The selected design arms a backoff at K=1 too, because the
  schedule is uniform across K. Operators who want today's "no backoff
  at K=1" behavior set `backoff_initial = Duration::ZERO`. Worth a
  release-note bullet.

- **Time source.** `run_health_check_cycle` already takes `now:
  DateTime<Utc>` as a parameter (`health_checker.rs:194`), so synthetic
  time at the cycle level is trivial. `step_service` will follow the
  same convention. No `tokio::time::Instant` plumbing is needed.

- **`failure_count` overflow.** `u32` failureCount is overkill; the
  realistic upper bound is "as many cycles as fit between `backoff_initial`
  and `backoff_max`," which is `log2(backoff_max / backoff_initial)`. Use
  `saturating_add` and `saturating_mul` everywhere; no panic path.

- **Lean rows where `start_count > 0` but `start_state ∈ {healthy,
  reconnecting}`.** These are unreachable in a real trace (the global
  invariant rules them out) but the Lean enumeration emits them anyway
  because `step?` is total. The conformance test sets the fields
  literally per the row and asserts the per-step transition — this is
  the intended semantics, matching how K=1 rows like `evicted_0_*` are
  already handled.

- **No Properties witness emitted.** H5 / H6 / H6' / H7 stay Lean-only.
  H5 is observable at the cycle level (Layer 2 test asserts the
  inter-eviction probeFail count is ≥ K), but the assertion is
  cycle-test-local, not a JSON witness row. Promoting H5 to a
  `consumerCoverage` Properties row is a follow-up; the audit's "Lean
  only / followUpCoverage" pattern from PR #255 is the template.

- **Coupling.lean C1–C4.** Out of scope. The audit treats this as a
  separate gap (preflight is unwired in `mcp_pool.call_tool`). Once a
  later spec wires `preflight`, the C1–C4 theorems can be witnessed by
  a `Coupling` row separate from `mcp_health_cases`.

## Out of Scope

- Wiring `ToolExecution.preflight` in `mcp_pool.call_tool` (Coupling C1–C4).
- Changing the Lean spec.
- Persisting K-state to DefraDB or gossiping it.
- Per-service K configuration via `ToolServiceRegistry`.
- Operator-facing CLI flags or config files for `HealthCheckerOptions`
  beyond the struct itself (today only `DefraAgent::builder()` callers
  override; surfacing through `defra-agent-cli` is a follow-up).
