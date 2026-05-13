# MCP health probe / eviction state machine in Lean: design

**Status:** Design
**Date:** 2026-05-13
**Tracks:** issue #186 (this work); parent #183 (formal coverage audit follow-ups); audit `docs/superpowers/audits/2026-05-13-formal-coverage-audit.md` gap #7.
**Scope:** A small per-service Lean state machine over `healthy → degraded → evicted → reconnecting`, parameterized by a failure-count threshold K. Includes a projection lemma that bridges the new state machine into the existing `Proofs/ToolExecution/Policy.lean` preflight contract, plus conformance witnesses for today's K=1 Rust behavior. No Rust production-code changes. No new TLA+ artifact.

## 1. Goal

Close audit gap #7. The audit's leverage statement:

> probe interval, staleness window, eviction lifecycle, and pool eviction state machine are operational only ... modeling probe/eviction as a state machine would catch flapping-connection-pool bugs before they reach prod; leaving it open means health-driven pool eviction has no contract beyond Rust tests.

The bug class the audit names — **flapping** — is what makes #186 worth doing relative to its low immediate stakes. A formal **inter-eviction gap** property gives the runtime a contract that catches "one bad probe evicts; the next call lazily reconnects; one bad probe evicts; ..." regression as a proof failure rather than an integration-test gotcha.

This spec produces an implementation plan, not implementation. The plan is the next step (writing-plans skill).

## 2. Why now

- Every connection-pool bug shipped historically has been health-related.
- The existing `Proofs/ToolExecution/Policy.lean` already encodes a 3-value `Health` ADT (`healthy`/`stale`/`unreachable`) and a `preflight` decision keyed on it, but **no state machine** — no transitions, no flapping property, no anti-oscillation guarantee.
- Issue #186 explicitly chose a richer 4-state shape (`healthy → degraded → evicted → reconnecting`) than the 3-state code today. That ask is structurally inconsistent with the current Rust health checker (which evicts on a single probe failure) unless we parameterize: a failure threshold `K` lets `K=1` describe today's behavior and `K≥2` describe the anti-flapping regime.
- The MCP boundary is increasingly central as more services move to MCP (`x-data`, `hf-data`, `coding-session`, observability, etc.); a formal probe/eviction contract scales the safety we already prove on the tool-call lifecycle out to the connection lifecycle.

## 3. Acceptance against #186

| Acceptance item | Where it lands |
|---|---|
| Four-state machine `healthy → degraded → evicted → reconnecting` with bounded transitions | `Proofs/MCPHealth/State.lean` + `Proofs/MCPHealth/Transition.lean` |
| `Healthy → Degraded`: probe fails or staleness threshold crossed | `step?` on `probeSuccess(staleness=true)` and `probeFail` with `K≥2` |
| `Degraded → Evicted`: N consecutive probe failures | `step?` on `probeFail` with `failureCount + 1 ≥ K` |
| `Evicted → Reconnecting`: explicit re-add or backoff expiry | `step?` on `backoffExpiry` |
| `Reconnecting → Healthy`: successful probe | `step?` on `probeSuccess(false)` from `.reconnecting` |
| Coupling to `ToolRetryDisposition` | **Reframed** (§5): the load-bearing coupling is to `preflight`, not `retryDisposition`. See §5 for the rationale. |
| Flapping bound (the load-bearing property) | `h5_anti_flapping_inter_eviction_gap` in `Proofs/MCPHealth/Properties.lean` |
| Probe/eviction event ordering: every transition triggered by a named event | `h1_event_triggered` + the fact that `step?` is a total function over `Event` |
| Conformance vocabulary registered | `Proofs/MCPHealth/Executable.lean` exports K=1 rows for Rust; K≥2 rows emitted but not yet consumed |
| Rust consumer tests probe/eviction transitions against the proven model | New test in `crates/defra-agent/src/health_checker.rs` (`#[cfg(test)] mod tests`) consuming K=1 rows. **Tests only — no production-code changes.** |

## 4. Architecture

### 4.1 Files

Module lives at `crates/defra-agent/proofs/Proofs/MCPHealth/` with the conventional 4-file split used by `Proofs/ToolExecution/`, `Proofs/Request/`, and `Proofs/Triggers/`, plus a small `Coupling.lean` to isolate the dependency on `Proofs/ToolExecution/Policy.lean`:

```
Proofs/MCPHealth.lean              -- entry: re-exports the namespace
Proofs/MCPHealth/State.lean        -- HealthState, ServiceModel, Event
Proofs/MCPHealth/Transition.lean   -- step?, run?
Proofs/MCPHealth/Properties.lean   -- H1–H8
Proofs/MCPHealth/Coupling.lean     -- healthProjection + preflight bridge
Proofs/MCPHealth/Executable.lean   -- conformance witness rows
```

Dependency edges:

```
MCPHealth.State        --→ Proofs.Basic
MCPHealth.Transition   --→ MCPHealth.State
MCPHealth.Properties   --→ MCPHealth.Transition
MCPHealth.Coupling     --→ MCPHealth.State + Proofs.ToolExecution.Policy
MCPHealth.Executable   --→ MCPHealth.Transition + MCPHealth.Coupling
MCPHealth (entrypoint) --→ all of the above
```

### 4.2 `Proofs.lean` import

Add a single line:

```lean
import Proofs.MCPHealth
```

Insertion site: between line 18 (`import Proofs.ToolExecution`) and line 19 (`import Proofs.Subagent`). The Coupling.lean dependency on `Proofs.ToolExecution.Policy` is satisfied by ordering after the ToolExecution import.

**Coordination note.** Per the brief, five sibling streams (#191, #188, #189, #187, #185) may also edit `Proofs.lean`. The only shared editing surface is the import list. Last-to-land rebases.

### 4.3 Per-service scope

Both the Rust connection pool (`crates/defra-agent/src/mcp_pool.rs`: `HashMap<service_id, McpConnection>`) and health map (`crates/defra-agent/src/health_checker.rs`: `HashMap<service_id, ServiceHealth>`) are keyed by `service_id` with no cross-service coupling beyond the registry retain set. The Lean state machine matches: one `ServiceModel` per service. Aggregate-pool properties would multiply bookkeeping without closing any cross-service invariant.

The registry-retain rule ("services not in the online set get dropped") is modeled at the event layer (`Event.registryAbsent` returns `none` from `step?`); no separate Pool.lean wrapper.

### 4.4 Parameterization over K

A single `Threshold` parameter governs whether a probe failure goes straight to `Evicted` (K=1, today) or transits `Degraded` first (K≥2, future):

| K | Meaning | Witnessed in Rust today? |
|---|---|---|
| `K = 1` | One probe failure → `Evicted` immediately. `Degraded` is reachable only via `probeSuccess(staleness=true)` (mirrors today's `Stale`). | **Yes** — conformance rows witness this against `run_health_check`. |
| `K ≥ 2` | Probe failures accumulate in `Degraded` (`failureCount < K`) before evicting. `backoffExpiry` becomes a meaningful event. | **No** — conformance rows are emitted but not consumed; they form the formal contract for a future Rust K-aware refactor. |

This is the load-bearing choice that lets the spec close #186's acceptance criteria without forcing a Rust refactor in the same PR.

## 5. ToolRetryDisposition coupling

Issue #186 names `ToolRetryDisposition` as the coupling target. The brief asks us to "brainstorm to confirm this is the right vocabulary." We propose **reframing the coupling target to `preflight`, not `retryDisposition`**, on these grounds:

- `retryDisposition : ToolOperation → IdempotencyEvidence → FailureClass → RetryDisposition` is keyed on what kind of operation failed, not on what state the service is in. Adding `HealthState` as an input would break that orthogonality and conflict with the "additive only" coordination rule for `ToolExecution/Policy.lean`.
- The operationally-relevant decision when a service is `Evicted` or `Reconnecting` is **dispatch admission**, not retry classification: today's `enforce_health_gate` (`crates/defra-agent/src/meta_tools/shared.rs`) blocks dispatch before any retry decision is reached. That gate's contract is `preflight`.
- The retry behavior today (`list_tools` lazily reconnects after a pool eviction; `call_tool` does not retry) is a property of the operation and its idempotency, and is already proven by `mcp_call_without_idempotency_metadata_does_not_retry` / `list_tools_transport_retry_is_safe_read`. Nothing about that changes when we add the state machine.

So the new file `Coupling.lean` defines a projection `healthProjection : HealthState → ToolExecution.Health`:

```
.healthy        ↦ .healthy
.degraded       ↦ .stale
.evicted        ↦ .unreachable
.reconnecting   ↦ .unreachable
```

And proves four lemmas (C1–C4) that compose the projection with `preflight`. Nothing inside `ToolExecution/Policy.lean` changes. The retry vocabulary stays orthogonal and unchanged.

The reframed coupling still discharges the spirit of #186's ask ("an evicted service implies the documented dispatch-blocking behavior") with the right vocabulary.

## 6. Core types

### 6.1 `HealthState`

```lean
inductive HealthState where
  | healthy
  | degraded
  | evicted
  | reconnecting
  deriving DecidableEq, Repr
```

### 6.2 `ServiceModel`

```lean
structure ServiceModel where
  state        : HealthState
  failureCount : Nat
  deriving DecidableEq, Repr
```

`failureCount` is the count of consecutive `probeFail` events since the last `probeSuccess`. Carrying it on the structure (rather than inside `.degraded`) lets `Reconnecting → probeFail → Evicted` increment without re-deriving the count from `.degraded`'s payload.

**`Degraded` is a single constructor with two semantic flavors, distinguished by `failureCount`:**

| `failureCount` | Flavor | Entered by |
|---|---|---|
| `0` | **Staleness-degraded.** The last probe succeeded but the heartbeat is older than the staleness window. Operationally equivalent to today's `Stale`. | `probeSuccess(staleness = true)` from any state. |
| `≥ 1` | **Failure-count-degraded.** Saw `failureCount` consecutive probe failures, but `failureCount < K` so the service is not yet evicted. Only reachable under `K ≥ 2`. | `probeFail` from `Healthy`/`Degraded`/`Reconnecting` when `failureCount + 1 < K`. |

The two flavors share `Degraded` (and therefore share `healthProjection .degraded = .stale`) because the operational dispatch decision is identical: both flavors admit `call_tool` with a longer timeout (matching today's "stale services allowed through with a longer timeout" behavior). The doc comment on `ServiceModel.failureCount` will state this distinction explicitly.

### 6.3 `Event`

```lean
inductive Event where
  | probeSuccess (staleness : Bool)
  | probeFail
  | backoffExpiry
  | registryAbsent
  deriving DecidableEq, Repr
```

Staleness is a probe-success modifier (mirrors `health_checker.rs:247` where `is_stale` is computed inline from the heartbeat age at probe time). `probeFail` folds both error and timeout — operationally identical in Rust (`health_checker.rs:268,:289` both `mcp_pool.remove` and set `Unreachable`). `backoffExpiry` is a no-op outside `.evicted`; `registryAbsent` removes the entity from the model entirely (returns `none` from `step?`).

`Event.all` enumerates all five inhabitants (`probeSuccess false`, `probeSuccess true`, `probeFail`, `backoffExpiry`, `registryAbsent`) for use in conformance generation.

## 7. Transitions

### 7.1 `step?`

```lean
abbrev Threshold := { k : Nat // k ≥ 1 }

def step? (sm : ServiceModel) (e : Event) (K : Threshold) : Option ServiceModel :=
  match e with
  | .registryAbsent => none
  | .backoffExpiry  =>
      some { sm with state := if sm.state = .evicted then .reconnecting else sm.state }
  | .probeSuccess stale =>
      some { state := if stale then .degraded else .healthy
           , failureCount := 0 }
  | .probeFail =>
      let n := sm.failureCount + 1
      if n ≥ K.val then some { state := .evicted,  failureCount := n }
                   else some { state := .degraded, failureCount := n }
```

**Design choice: `Reconnecting` is an *optional* pass-through state, not mandatory.**

`probeSuccess(false)` from `Evicted` returns directly to `Healthy` (skipping `Reconnecting`) rather than `none`. We deliberately choose the **permissive** version on these grounds:

- **K=1 (today) has no `Reconnecting` state in Rust.** When a service is evicted, the next health-check tick simply calls `mcp_pool.list_tools(...)`, which lazily reconnects via `get_or_connect`. A successful probe assigns `HealthStatus::Healthy` directly. There is no intermediate observable state. Forcing the path through `Reconnecting` would make every K=1 conformance row for `Evicted + probeSuccess` fail against today's Rust.
- **`Reconnecting` is meaningful only under K≥2 with an armed backoff.** In that regime, `Evicted → backoffExpiry → Reconnecting → probeSuccess → Healthy` is the natural sequence; the backoff is what introduces the intermediate state. Without an armed backoff, there's nothing between "I was evicted" and "the next probe succeeded."
- **Restrictive alternative considered and rejected.** Returning `none` (or holding state in `Evicted`) on `Evicted + probeSuccess` would force the path through `Reconnecting`, but at the cost of either making the K=1 path unreachable (the service can never recover without a backoff event) or requiring Rust to emit a `backoffExpiry` event before every successful reconnect (gratuitous mechanism). Neither matches today's Rust.

This is documented in property **H6'** (§8) and in the doc comment on `step?`.

### 7.2 `run?`

```lean
def run? (sm : ServiceModel) (events : List Event) (K : Threshold)
    : Option ServiceModel :=
  events.foldl (fun acc e => acc.bind (fun sm' => step? sm' e K)) (some sm)
```

`run?` short-circuits on the first `.registryAbsent` — the per-service state machine ends, and any later events do not apply (analogous to terminal-irreversibility in the request lifecycle).

### 7.3 K=1 collapse table

For K=1, the matrix evaluates to:

| start state | event | next state | failureCount |
|---|---|---|---|
| Healthy | probeSuccess false | Healthy | 0 |
| Healthy | probeSuccess true | Degraded | 0 |
| Healthy | probeFail | **Evicted** | 1 |
| Healthy | backoffExpiry | Healthy | unchanged |
| Healthy | registryAbsent | (removed) | — |
| Degraded | probeSuccess false | Healthy | 0 |
| Degraded | probeSuccess true | Degraded | 0 |
| Degraded | probeFail | **Evicted** | n+1 |
| Degraded | backoffExpiry | Degraded | unchanged |
| Degraded | registryAbsent | (removed) | — |
| Evicted | probeSuccess false | Healthy [†] | 0 |
| Evicted | probeSuccess true | Degraded [†] | 0 |
| Evicted | probeFail | Evicted | n+1 |
| Evicted | backoffExpiry | **Reconnecting** | unchanged |
| Evicted | registryAbsent | (removed) | — |
| Reconnecting | probeSuccess false | Healthy | 0 |
| Reconnecting | probeSuccess true | Degraded | 0 |
| Reconnecting | probeFail | **Evicted** | n+1 |
| Reconnecting | backoffExpiry | Reconnecting | unchanged |
| Reconnecting | registryAbsent | (removed) | — |

At K=1, every `probeFail` from a non-removed state goes to `Evicted` in one step — matching today's Rust single-failure eviction. The `Degraded`-as-stale-only path matches the `Stale` HealthStatus today.

[†] The `Evicted → Healthy` and `Evicted → Degraded` rows on `probeSuccess` reflect the **permissive recovery** choice (§7.1): `Reconnecting` is an optional pass-through, not mandatory. Under K=1, Rust has no `Reconnecting` state, so a successful probe after eviction assigns `Healthy` directly. Property **H6'** formalizes this.

## 8. Properties

| Tag | Statement | Kind | Discharged in |
|---|---|---|---|
| H1 | Every next-state arises from a named `Event`. No spontaneous transitions. | Safety (structural) | `Properties.lean` |
| H2 | `probeSuccess` resets `failureCount` to 0. | Arithmetic | `Properties.lean` |
| H3 | `probeFail` increments `failureCount` by exactly 1. | Arithmetic | `Properties.lean` |
| H4 | `backoffExpiry` is a no-op outside `.evicted`. | Safety | `Properties.lean` |
| **H5** | **Anti-flapping inter-eviction gap:** if `run?` reaches `.healthy` at prefix p1 and `.evicted` at later prefix p2, then the event slice `events[p1..p2]` contains ≥ K probeFail events. | **Safety (load-bearing)** | `Properties.lean` |
| H6 | From `.evicted`, the two-event sequence `[backoffExpiry, probeSuccess false]` reaches `.healthy`. | Liveness (constructive) | `Properties.lean` |
| H6' | From `.evicted`, a single `probeSuccess false` reaches `.healthy` directly (permissive transition; the `Reconnecting` pass-through is optional). Witnesses the K=1 collapse where Rust has no `Reconnecting` state. | Liveness (constructive) | `Properties.lean` |
| H7 | At K=1, a `probeFail` from any non-degraded state with `failureCount = 0` goes directly to `.evicted` (witnesses the K=1 collapse to today's Rust). | Safety | `Properties.lean` |
| H8 | `registryAbsent` ends the per-service state machine (`step? sm .registryAbsent K = none`). | Safety | `Properties.lean` |
| C1 | `preflight (healthProjection .evicted) schema = .block .serviceUnavailable`. | Coupling | `Coupling.lean` |
| C2 | `preflight (healthProjection .reconnecting) schema = .block .serviceUnavailable`. | Coupling | `Coupling.lean` |
| C3 | `preflight (healthProjection .healthy) schema = .dispatch` when `schema ≠ .invalid`. | Coupling | `Coupling.lean` |
| C4 | `preflight (healthProjection .degraded) schema = .dispatch` when `schema ≠ .invalid`. | Coupling | `Coupling.lean` |

H5 is the **load-bearing safety property** — the contract that catches flapping regressions.

## 9. Conformance witnesses

`Executable.lean` emits `TransitionCase` rows:

```lean
structure TransitionCase where
  name            : String   -- "transition_K1_healthy_probeFail_evicted_1" etc.
  startState      : HealthState
  startCount      : Nat
  event           : Event
  thresholdK      : Nat
  nextState       : Option HealthState   -- none = service removed by registryAbsent
  nextCount       : Option Nat
  rustProjection  : Option String        -- "healthy" | "stale" | "unreachable" | none
  deriving Repr
```

Generation is exhaustive over `K ∈ {1, 2, 3}`, `startState ∈ HealthState.all`, `startCount ∈ {0..K}`, `event ∈ Event.all`. The `rustProjection` field applies `healthProjection ∘ ToolExecution.Health.toDefraDB` and is `Some` whenever `nextState` is `some _`.

Two derived projections are exposed:

- `k1ProjectionCases : List TransitionCase` — filtered to `thresholdK = 1`. **Consumed by Rust today.**
- `k2PlusFutureCases : List TransitionCase` — `thresholdK ≥ 2`. **Emitted but not yet consumed by a Rust assertion.** These are the formal contract for a future K-aware Rust refactor.

A single Lean theorem ties the K=1 enumeration to today's Rust observable behavior:

```lean
theorem k1_cases_match_rust_health_status :
    ∀ row ∈ k1ProjectionCases, rustProjectionAgreesWithCurrentRustBehavior row
```

`rustProjectionAgreesWithCurrentRustBehavior` is a structural predicate over the row's `rustProjection` field; the body is `rfl` on each enumerated row.

## 10. Rust consumer (test-only)

Add one new test to `crates/defra-agent/src/health_checker.rs` (`#[cfg(test)] mod tests`). The test is parameterized by the Lean-generated K=1 rows via the existing `lean_vocab_test` bridge module pattern (mirrors `lean_tool_preflight_cases` from `meta_tools/call.rs:367`):

```rust
#[tokio::test]
async fn generated_mcp_health_k1_cases_match_health_checker_transitions() {
    for case in lean_mcp_health_k1_cases() {
        let actual = simulate_health_checker_one_event(&case).await;
        assert_eq!(actual, case.rust_projection,
            "Lean MCPHealth K=1 case {} must match Rust HealthStatus", case.name);
    }
}
```

The Rust bridge file (`crates/defra-agent/src/lean_vocab_test.rs` or a sibling) gets one new struct (`LeanMcpHealthCase`) and one new helper (`lean_mcp_health_k1_cases()`). **The bridge file is test-only (`#[cfg(test)]`)**; this is not production code.

The `simulate_health_checker_one_event` helper drives a single probe outcome through a stripped-down version of `run_health_check`'s decision logic, using a `ServiceHealthMap` and a controlled probe result. **No changes to `run_health_check` itself.**

### 10.1 Out of scope for the Rust test

- Asserting K=2 transitions (Rust has no K≥2 behavior to assert against).
- Asserting the `c1`–`c4` preflight lemmas in Rust (already covered by `generated_tool_preflight_cases_match_health_and_schema_gates` in `meta_tools/call.rs:385`; we don't duplicate).
- Driving real probes against a real MCP server (the test uses a deterministic event vocabulary; integration coverage is upstream).

## 11. What this spec is not

- **Not a Rust refactor.** Production code in `health_checker.rs` and `mcp_pool.rs` is untouched. The K≥2 regime is described formally so that a future Rust refactor has a contract to satisfy, but the refactor itself is out of scope.
- **Not a TLA+ artifact.** The state machine is per-service and event-driven; there is no cross-node coordination to motivate a temporal-logic spec. Sibling stream #188 is the TLA+ track.
- **Not a backoff-timer model.** `backoffExpiry` is an external event; the model does not specify durations or backoff strategy. Future work may introduce a separate `Proofs/MCPHealth/Backoff.lean` if Rust grows a configurable backoff schedule, but that is not required by #186.
- **Not a model of probe interval, staleness window, or probe timeout durations.** The relevant constants in `health_checker.rs:23,:24,:25` (30 s / 120 s / 5 s) are values used by `run_health_check` to compute the `staleness` flag and to issue the probe; the state machine consumes the flag and the probe outcome but doesn't itself encode durations. This mirrors how `Proofs/Request/*` treats deadlines as parameters rather than modeling wall-clock time.
- **Not a change to `Proofs/ToolExecution/Policy.lean`.** Additive extension via `Coupling.lean` only.

## 12. Hard constraints honored

| Constraint (from brief) | Honored by |
|---|---|
| Zero `sorry` | All proofs are by `rfl` / structural induction / arithmetic. H5's induction is over the event list. |
| No Rust production code | Only `#[cfg(test)]` additions to `health_checker.rs` and one new test-only helper in `lean_vocab_test.rs`. |
| Don't model the network or the MCP protocol | The model treats probes as event-tagged transitions. Wire-level concerns (HTTP, transport, rmcp protocol) are out of scope. |
| Don't conflict with `ToolExecution/Policy.lean` | `Coupling.lean` is the only file that imports `ToolExecution.Policy`. No edits to `Policy.lean`. |
| Coordinate `Proofs.lean` imports | Named insertion site (between lines 18 and 19). Last-to-land rebases. |

## 13. Coordination with sibling streams

| Stream | Likely edit surface | Conflict risk with this stream |
|---|---|---|
| #191 (`Session/Transcript`) | New `Proofs/Session/Transcript.lean` or `Proofs/Transcript/`; `Proofs.lean` import line | Low — disjoint files; only `Proofs.lean` overlaps. |
| #188 (TLA+) | `proofs/tla/` only | None — different artifact tree. |
| #189 (`Liveness`/`Recovery`) | `Proofs/Properties/Liveness.lean` + `Proofs/Recovery/`; `Proofs.lean` | Low — disjoint files. |
| #187 (`EventDelivery`) | `Proofs/EventDelivery/` or Triggers extension; `Proofs.lean` | Low — disjoint files. |
| #185 (`Identity`) | `Proofs/Identity/`; `Proofs.lean` | Low — disjoint files. |

**The only shared editing surface across all six streams is `Proofs.lean`.** This spec adds exactly one line (`import Proofs.MCPHealth`); any conflict is a trivial rebase.

`Proofs/ToolExecution/Policy.lean` is named in the brief as "stable today; extension is fine; refactor is not." This spec is purely additive (a new `Coupling.lean` that imports `Policy`, never modifies it). Same rule applies symmetrically to sibling streams.

## 14. Risks and known limitations

- **K is a free parameter in the model but pinned at 1 in Rust today.** A future K≥2 refactor must (a) add a per-service failure counter to `ServiceHealth`, (b) introduce a `Degraded` variant to `HealthStatus`, (c) gate `mcp_pool.remove` on `failureCount ≥ K`, (d) consume the K≥2 conformance rows in a new test. This spec defines that contract; it does not deliver the refactor.
- **`backoffExpiry` semantics are minimal.** The model only requires "evicted + backoffExpiry → reconnecting." It does not model arming, cancellation, jitter, or schedule choice. Future Rust may add backoff timers; the model accommodates that by leaving the event's timing unspecified.
- **Permissive `Evicted → Healthy` recovery (see §7.1, H6').** The state machine admits a direct `probeSuccess` from `Evicted` to `Healthy`, skipping `Reconnecting`. This is the right model of today's Rust, but it also means the state machine does **not** enforce "every recovery must pass through `Reconnecting`." If a future Rust refactor wants `Reconnecting` to be observable (e.g., for runtime status counters), it must also change `step?` to return `none` on `Evicted + probeSuccess`, drop H6', and route every recovery through `backoffExpiry`. That refactor is out of scope here.
- **`registryAbsent` is modeled as terminal.** In Rust, a service can disappear from the online set and reappear later with the same `service_id`. The model treats reappearance as a fresh `ServiceModel` (i.e., `step?` returns `none`, and the pool layer would seed a new model). If we want to formalize same-id resurrection, that would need a separate Pool.lean wrapper — explicitly out of scope here.
- **Staleness is observational, not driven by an async tick.** Today's Rust computes staleness inline from the heartbeat age at probe time, which means the model never needs an async time event. If Rust ever moves to an asynchronous staleness flagger (independent of probes), the event vocabulary would need a fifth event — this spec accepts that small future delta.
- **H5's induction depth.** The flapping-bound theorem is a quantifier over arbitrary event lists. The proof is by induction on the suffix `events[p1..p2]`; cases are `nil` (vacuous), `probeSuccess _ :: …` (impossible to reach `.evicted` since it resets `failureCount`), `probeFail :: …` (decrement K by 1 and recurse), `backoffExpiry :: …` and `registryAbsent :: …` (vacuous transit). Lemma `failureCount_le_K_along_degraded` discharges the bookkeeping.

## 15. Definition of done

- [ ] All five `MCPHealth/*.lean` files written, zero `sorry`.
- [ ] H1–H8 and C1–C4 closed.
- [ ] `Proofs.lean` carries `import Proofs.MCPHealth` (one line).
- [ ] `lake build` clean from `crates/defra-agent/proofs/`.
- [ ] `Proofs/MCPHealth/Executable.lean` enumerates K∈{1,2,3} cases; `k1ProjectionCases` matches `transitionCases.filter (·.thresholdK = 1)`.
- [ ] New `#[cfg(test)]` consumer in `health_checker.rs` passes; K=1 row count > 0 and asserted.
- [ ] `cargo test -p defra-agent` clean (no production-code changes; only `#[cfg(test)]` additions).
- [ ] No edits to `Proofs/ToolExecution/Policy.lean`.

## 16. Out-of-scope follow-ups (not this PR)

- **K≥2 Rust refactor.** When and if Rust adopts N-consecutive-failures eviction, a follow-up issue can drive the changes; the K≥2 conformance rows are pre-positioned.
- **Backoff timer modeling.** If Rust grows a configurable backoff strategy, add `Proofs/MCPHealth/Backoff.lean` and the corresponding properties.
- **Pool-level theorems.** If a cross-service invariant arises (e.g., "no more than M services evicted simultaneously"), a `Proofs/MCPHealth/Pool.lean` could land. Today's code has no such constraint.
- **Cross-deployment MCP service propagation.** If MCP services start gossiping their health across deployments, that's a TLA+ track sibling to #176 and #162, not a Lean extension.
