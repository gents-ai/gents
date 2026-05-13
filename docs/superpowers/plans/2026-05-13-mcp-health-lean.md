# MCP Health Probe / Eviction State Machine in Lean Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land a per-service Lean state machine over `healthy → degraded → evicted → reconnecting`, parameterized by a failure-count threshold K, with a load-bearing anti-flapping inter-eviction-gap theorem, projection coupling to `Proofs/ToolExecution/Policy.lean`, conformance witnesses for today's K=1 Rust behavior, and one new `#[cfg(test)]` consumer test in `crates/defra-agent/src/health_checker.rs`.

**Architecture:** Five Lean files under `crates/defra-agent/proofs/Proofs/MCPHealth/` plus one entrypoint file. JSON conformance emission via `Proofs/Conformance/Contracts/Json.lean` (additive — one new field, one new emitter helper). Rust side: one new case struct + one accessor in `crates/defra-agent/src/lean_vocab_test.rs`, plus the consumer test in `crates/defra-agent/src/health_checker.rs`. No Rust production code changes (no edits to `HealthStatus`, `ServiceHealth`, `run_health_check`, or `mcp_pool.rs`). No edits to `Proofs/ToolExecution/Policy.lean`.

**Tech Stack:** Lean 4 (via `lake`); Rust (via `cargo`); `serde` + `serde_json` for the Lean→Rust JSON bridge.

**Reference:** `docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md` is the source of truth for shapes, names, and acceptance.

---

## File Structure

**New files (Lean):**
- `crates/defra-agent/proofs/Proofs/MCPHealth.lean` — entry; re-exports the namespace.
- `crates/defra-agent/proofs/Proofs/MCPHealth/State.lean` — `HealthState`, `ServiceModel`, `Event`, vocabularies.
- `crates/defra-agent/proofs/Proofs/MCPHealth/Transition.lean` — `Threshold`, `step?`, `run?`.
- `crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean` — H1–H8, H6'.
- `crates/defra-agent/proofs/Proofs/MCPHealth/Coupling.lean` — `healthProjection`, C1–C4.
- `crates/defra-agent/proofs/Proofs/MCPHealth/Executable.lean` — `TransitionCase` + `transitionCases` + filters.

**Modified files (Lean):**
- `crates/defra-agent/proofs/Proofs.lean` — add one import line.
- `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean` — add `mcpHealthCaseJson` + one new field in the top-level JSON emitter.
- `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` — add one consumer-coverage entry.

**Modified files (Rust, test-only):**
- `crates/defra-agent/src/lean_vocab_test.rs` — add `LeanMcpHealthCase` struct, add `mcp_health_cases` field on `LeanContractSnapshot`, add `lean_mcp_health_k1_cases()` accessor.
- `crates/defra-agent/src/health_checker.rs` — add one new test inside the existing `#[cfg(test)] mod registry_parsing_tests` (or a sibling `mod transitions_tests`).

---

## Task 1: Scaffold the MCPHealth module and wire it into Proofs.lean

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/MCPHealth.lean`
- Create: `crates/defra-agent/proofs/Proofs/MCPHealth/State.lean` (empty namespace)
- Modify: `crates/defra-agent/proofs/Proofs.lean` (one new import)

- [ ] **Step 1: Create the stub `State.lean`** (empty namespace, to establish the directory):

```lean
namespace Proofs.MCPHealth

-- types defined in subsequent tasks

end Proofs.MCPHealth
```

Path: `crates/defra-agent/proofs/Proofs/MCPHealth/State.lean`

- [ ] **Step 2: Create the entrypoint `MCPHealth.lean`:**

```lean
import Proofs.MCPHealth.State

-- Subsequent imports added as tasks land:
-- import Proofs.MCPHealth.Transition
-- import Proofs.MCPHealth.Properties
-- import Proofs.MCPHealth.Coupling
-- import Proofs.MCPHealth.Executable
```

Path: `crates/defra-agent/proofs/Proofs/MCPHealth.lean`

- [ ] **Step 3: Add the new import to `Proofs.lean`:**

Modify `crates/defra-agent/proofs/Proofs.lean`. Between line 18 (`import Proofs.ToolExecution`) and line 19 (`import Proofs.Subagent`), insert:

```lean
import Proofs.MCPHealth
```

Final line 19 becomes the new MCPHealth import; the old line 19 (`import Proofs.Subagent`) shifts to line 20. Verify with `head -22 crates/defra-agent/proofs/Proofs.lean`.

- [ ] **Step 4: Verify `lake build` is clean.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: build succeeds with no errors, no warnings about `sorry`.

- [ ] **Step 5: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth.lean \
        crates/defra-agent/proofs/Proofs/MCPHealth/State.lean \
        crates/defra-agent/proofs/Proofs.lean
git commit -m "Scaffold Proofs.MCPHealth module (#186)"
```

---

## Task 2: State.lean — HealthState, ServiceModel, Event

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth/State.lean`

- [ ] **Step 1: Write the full `State.lean` body.**

Replace the empty namespace from Task 1 with:

```lean
import Proofs.Basic

/-!
# MCP Health / Eviction — Types

Per-service state machine for the MCP connection-pool health checker. See
`docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md` for the design.

`HealthState` is the four-state lifecycle from #186. `ServiceModel` carries the
state plus a `failureCount` that distinguishes the two semantic flavors of
`Degraded` (see doc comment). `Event` is the named-event vocabulary that
drives transitions; no async tick is modeled.
-/

namespace Proofs.MCPHealth

/-- The four-state lifecycle.

    `healthy` — last probe succeeded with fresh heartbeat.
    `degraded` — last probe succeeded but heartbeat is stale (`failureCount = 0`),
                 or saw `failureCount ≥ 1` consecutive failures with
                 `failureCount < K` (only under K ≥ 2).
    `evicted` — pool connection has been removed; no calls admitted.
    `reconnecting` — backoff expired after eviction; awaiting next probe.
                     Unreachable at K=1 / no backoff. -/
inductive HealthState where
  | healthy
  | degraded
  | evicted
  | reconnecting
  deriving DecidableEq, Repr

namespace HealthState

def toDefraDB : HealthState → String
  | .healthy      => "healthy"
  | .degraded     => "degraded"
  | .evicted      => "evicted"
  | .reconnecting => "reconnecting"

def all : List HealthState :=
  [ .healthy, .degraded, .evicted, .reconnecting ]

theorem all_complete (s : HealthState) : s ∈ all := by
  cases s <;> simp [all]

end HealthState

/-- Per-service state model.

    `failureCount` is the count of consecutive `probeFail` events since the
    last `probeSuccess`. It is reset to 0 by any `probeSuccess` regardless of
    the `staleness` flag.

    The single `degraded` constructor has two semantic flavors distinguished
    by `failureCount`:

    * `failureCount = 0`: staleness-degraded (entered via
      `probeSuccess(staleness = true)`). Equivalent to today's `Stale`
      `HealthStatus`.
    * `failureCount ≥ 1`: failure-count-degraded (entered via `probeFail`
      when `failureCount + 1 < K`). Only reachable under K ≥ 2; unreachable
      under K=1 because `failureCount + 1 ≥ 1 = K` always evicts immediately.

    Both flavors share `healthProjection .degraded = .stale` and therefore
    the same preflight dispatch decision. -/
structure ServiceModel where
  state        : HealthState
  failureCount : Nat
  deriving DecidableEq, Repr

namespace ServiceModel

/-- Initial model — used when the pool first observes a service. -/
def initial : ServiceModel := { state := .healthy, failureCount := 0 }

end ServiceModel

/-- The four events that drive transitions.

    `probeSuccess` carries a `staleness : Bool` flag derived from the
    heartbeat age at probe time (mirrors `health_checker.rs:247`).

    `probeFail` folds both probe error and probe timeout — operationally
    identical in `health_checker.rs:268,:289` (both call `mcp_pool.remove`
    and set `Unreachable`).

    `backoffExpiry` is a no-op outside `.evicted`.

    `registryAbsent` removes the service from the model entirely
    (`step? sm .registryAbsent K = none`). -/
inductive Event where
  | probeSuccess (staleness : Bool)
  | probeFail
  | backoffExpiry
  | registryAbsent
  deriving DecidableEq, Repr

namespace Event

def toDefraDB : Event → String
  | .probeSuccess false => "probeSuccessFresh"
  | .probeSuccess true  => "probeSuccessStale"
  | .probeFail          => "probeFail"
  | .backoffExpiry      => "backoffExpiry"
  | .registryAbsent     => "registryAbsent"

def all : List Event :=
  [ .probeSuccess false, .probeSuccess true
  , .probeFail, .backoffExpiry, .registryAbsent ]

theorem all_complete (e : Event) : e ∈ all := by
  cases e
  · rename_i b; cases b <;> simp [all]
  all_goals simp [all]

end Event

end Proofs.MCPHealth
```

- [ ] **Step 2: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build, no `sorry`, no warnings.

- [ ] **Step 3: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/State.lean
git commit -m "Proofs.MCPHealth: define HealthState, ServiceModel, Event (#186)"
```

---

## Task 3: Transition.lean — Threshold, step?, run?

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/MCPHealth/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth.lean` (uncomment Transition import)

- [ ] **Step 1: Create `Transition.lean` with the full body.**

```lean
import Proofs.MCPHealth.State

/-!
# MCP Health / Eviction — Transitions

Deterministic event-driven transitions. `step?` is total over `Event`; it
returns `none` only on `registryAbsent` (service removed) and `some sm'`
otherwise. `run?` short-circuits on the first `registryAbsent`.

`Threshold` is the failure-count threshold K. K=1 collapses to today's Rust
behavior (single probeFail evicts); K ≥ 2 admits the bounded-flap regime.
-/

namespace Proofs.MCPHealth

/-- Failure-count threshold. K=1 today; K ≥ 2 admits the flapping-bound regime. -/
abbrev Threshold := { k : Nat // k ≥ 1 }

namespace Threshold

/-- K=1: today's Rust behavior (single probeFail → Evicted). -/
def one : Threshold := ⟨1, Nat.le.refl⟩

/-- Lift an arbitrary `Nat ≥ 1` to a `Threshold` (helpful in conformance). -/
def ofNat (k : Nat) (h : k ≥ 1) : Threshold := ⟨k, h⟩

end Threshold

/-- One transition step.

    `registryAbsent` returns `none` — the per-service state machine ends.
    `backoffExpiry` is a no-op outside `.evicted`.
    `probeSuccess stale` resets `failureCount` to 0 and routes to `.healthy`
    (fresh) or `.degraded` (stale).
    `probeFail` increments `failureCount` and routes to `.evicted` if the
    new count ≥ K, else `.degraded`. -/
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

/-- Sequential application of events. Short-circuits on `registryAbsent`. -/
def run? (sm : ServiceModel) (events : List Event) (K : Threshold)
    : Option ServiceModel :=
  events.foldl (fun acc e => acc.bind (fun sm' => step? sm' e K)) (some sm)

/-- `run? sm [] K = some sm`. -/
@[simp]
theorem run?_nil (sm : ServiceModel) (K : Threshold) :
    run? sm [] K = some sm := rfl

/-- One-event `run?` reduces to `step?`. -/
@[simp]
theorem run?_singleton (sm : ServiceModel) (e : Event) (K : Threshold) :
    run? sm [e] K = step? sm e K := by
  simp [run?, List.foldl, Option.bind]

end Proofs.MCPHealth
```

- [ ] **Step 2: Uncomment the Transition import in `MCPHealth.lean`.**

Modify `crates/defra-agent/proofs/Proofs/MCPHealth.lean` — uncomment the `Transition` line so it reads:

```lean
import Proofs.MCPHealth.State
import Proofs.MCPHealth.Transition

-- Subsequent imports added as tasks land:
-- import Proofs.MCPHealth.Properties
-- import Proofs.MCPHealth.Coupling
-- import Proofs.MCPHealth.Executable
```

- [ ] **Step 3: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build.

- [ ] **Step 4: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/Transition.lean \
        crates/defra-agent/proofs/Proofs/MCPHealth.lean
git commit -m "Proofs.MCPHealth: define Threshold, step?, run? (#186)"
```

---

## Task 4: Properties.lean — H1–H4 and H8 (simple safety + arithmetic)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth.lean` (uncomment Properties import)

- [ ] **Step 1: Create `Properties.lean` with H1–H4 and H8.**

```lean
import Proofs.MCPHealth.Transition

/-!
# MCP Health / Eviction — Properties

Safety, arithmetic, and liveness facts about `step?` / `run?`. Property
tags (H1–H8, H6') match the spec table in §8.

This file is built additively. Task 4 adds the easy safety/arithmetic facts;
later tasks add H7 (K=1 collapse), H6 / H6' (liveness), and H5 (anti-flapping
inter-eviction gap, load-bearing).
-/

namespace Proofs.MCPHealth

/-- H1: every legal next-state arises from a named `Event`; no spontaneous
    transitions. Trivially structural — `step?` is a total function of
    `Event`. Recorded as a fact for the audit's "no spontaneous transitions"
    acceptance criterion. -/
theorem h1_event_triggered
    (sm sm' : ServiceModel) (K : Threshold) :
    (∃ e, step? sm e K = some sm') ↔ sm' ∈ (Event.all.filterMap (fun e => step? sm e K)) := by
  constructor
  · rintro ⟨e, he⟩
    have := Event.all_complete e
    simp [List.mem_filterMap]
    exact ⟨e, this, he⟩
  · intro h
    simp [List.mem_filterMap] at h
    obtain ⟨e, _, he⟩ := h
    exact ⟨e, he⟩

/-- H2: `probeSuccess` resets `failureCount` to 0. -/
@[simp]
theorem h2_success_resets_failure_count
    (sm : ServiceModel) (stale : Bool) (K : Threshold) :
    (step? sm (.probeSuccess stale) K).map (·.failureCount) = some 0 := rfl

/-- H3: `probeFail` increments `failureCount` by exactly 1. -/
@[simp]
theorem h3_probefail_increments_count
    (sm : ServiceModel) (K : Threshold) :
    (step? sm .probeFail K).map (·.failureCount) = some (sm.failureCount + 1) := by
  simp [step?]
  split <;> rfl

/-- H4: `backoffExpiry` only changes state when starting from `.evicted`. -/
@[simp]
theorem h4_backoff_only_from_evicted (sm : ServiceModel) (K : Threshold) :
    (step? sm .backoffExpiry K).map (·.state)
      = some (if sm.state = .evicted then .reconnecting else sm.state) := rfl

/-- H8: `registryAbsent` ends the per-service state machine. -/
@[simp]
theorem h8_registry_absent_terminates
    (sm : ServiceModel) (K : Threshold) :
    step? sm .registryAbsent K = none := rfl

end Proofs.MCPHealth
```

- [ ] **Step 2: Uncomment the Properties import in `MCPHealth.lean`.**

Modify the entrypoint:

```lean
import Proofs.MCPHealth.State
import Proofs.MCPHealth.Transition
import Proofs.MCPHealth.Properties

-- Subsequent imports added as tasks land:
-- import Proofs.MCPHealth.Coupling
-- import Proofs.MCPHealth.Executable
```

- [ ] **Step 3: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build. No `sorry`. No warnings.

If any theorem step fails: check that `step?` is defined exactly as in Task 3, then re-run. The `split <;> rfl` pattern in H3 relies on the `if-then-else` inside `step?` having matching record shape on both branches.

- [ ] **Step 4: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean \
        crates/defra-agent/proofs/Proofs/MCPHealth.lean
git commit -m "Proofs.MCPHealth: prove H1–H4, H8 (event-triggered + arithmetic) (#186)"
```

---

## Task 5: Properties.lean — H7 (K=1 collapse) and helper lemmas

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean`

- [ ] **Step 1: Append H7 and a helper lemma to `Properties.lean`.**

At the end of the file, before `end Proofs.MCPHealth`, insert:

```lean
/-- H7: at K=1, `probeFail` from any non-removed state with `failureCount = 0`
    goes directly to `.evicted`. Witnesses the K=1 collapse to today's Rust
    single-failure eviction. -/
theorem h7_k1_collapse_probefail_skips_degraded
    (sm : ServiceModel) (h0 : sm.failureCount = 0)
    (K : Threshold) (hk : K.val = 1) :
    (step? sm .probeFail K).map (·.state) = some .evicted := by
  simp [step?, h0, hk]

/-- Helper: when `step?` lands in `.degraded` via `probeFail`, the new
    failureCount is strictly less than K. This is the bookkeeping invariant
    that supports H5's induction. -/
theorem degraded_count_lt_K
    (sm sm' : ServiceModel) (K : Threshold)
    (h : step? sm .probeFail K = some sm')
    (hd : sm'.state = .degraded) :
    sm'.failureCount < K.val := by
  simp [step?] at h
  split at h
  · -- n ≥ K branch: state becomes .evicted, contradicts hd = .degraded
    rename_i hge
    obtain ⟨hstate, _⟩ := Option.some_inj.mp h |>.symm |> (fun eq => ⟨congrArg _ eq, congrArg _ eq⟩)
    -- simpler: rewrite h, then derive state = .evicted from the record
    cases h
    simp at hd
  · -- n < K branch: this is the .degraded case
    rename_i hlt
    cases h
    -- sm'.failureCount = sm.failureCount + 1 = n
    -- hlt : ¬ (sm.failureCount + 1 ≥ K.val)
    omega
```

(Note for the executor: if the `degraded_count_lt_K` proof above doesn't compile cleanly, the structure is fine but the tactic script may need adjustment. The mathematical content is: split on the `if` inside `step?`; in the `≥ K` branch the state is `.evicted` which contradicts `hd`; in the `< K` branch the failure count is `sm.failureCount + 1` and `¬ (sm.failureCount + 1 ≥ K.val)` gives `sm.failureCount + 1 < K.val` by `omega`. Adjust to whichever Lean 4 idiom compiles.)

- [ ] **Step 2: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build. If `degraded_count_lt_K` fails, fix the tactic script — the mathematical content is sound; only the proof syntax may need adjustment.

- [ ] **Step 3: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean
git commit -m "Proofs.MCPHealth: prove H7 (K=1 collapse) and degraded_count_lt_K (#186)"
```

---

## Task 6: Properties.lean — H6 and H6' (constructive liveness)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean`

- [ ] **Step 1: Append H6 and H6' to `Properties.lean`.**

At the end of the file, before `end Proofs.MCPHealth`, insert:

```lean
/-- H6: from `.evicted`, the two-event sequence
    `[backoffExpiry, probeSuccess false]` reaches `.healthy`.
    Constructive liveness witness for the backoff-then-probe recovery path
    (relevant under K ≥ 2 with an armed backoff). -/
theorem h6_evicted_recovers_via_backoff_then_probe
    (sm : ServiceModel) (K : Threshold) (h : sm.state = .evicted) :
    (run? sm [.backoffExpiry, .probeSuccess false] K).map (·.state) = some .healthy := by
  simp [run?, List.foldl, Option.bind, step?, h]

/-- H6': from `.evicted`, a single `probeSuccess false` reaches `.healthy`
    directly (skipping `.reconnecting`). This is the **permissive** recovery
    path — `Reconnecting` is an optional pass-through state, not mandatory.

    Required by the K=1 conformance: today's Rust has no observable
    `Reconnecting` state, so a successful probe after eviction must assign
    `Healthy` directly. See spec §7.1 for the design rationale. -/
theorem h6'_evicted_recovers_via_probe_directly
    (sm : ServiceModel) (K : Threshold) (h : sm.state = .evicted) :
    (step? sm (.probeSuccess false) K).map (·.state) = some .healthy := rfl
```

- [ ] **Step 2: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build, no `sorry`.

- [ ] **Step 3: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean
git commit -m "Proofs.MCPHealth: prove H6, H6' (liveness — both recovery paths) (#186)"
```

---

## Task 7: Properties.lean — H5 (anti-flapping inter-eviction gap, load-bearing)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean`

This is the load-bearing safety theorem. The statement says: if a run reaches `.healthy` at prefix `p1` and `.evicted` at later prefix `p2`, the event slice `events[p1..p2]` contains ≥ K `probeFail` events.

- [ ] **Step 1: Append helper lemmas and H5 to `Properties.lean`.**

At the end of the file, before `end Proofs.MCPHealth`, insert:

```lean
/-! ## H5 — Anti-flapping inter-eviction gap (load-bearing)

The contract that catches the historical flapping connection-pool bug class.
For K=1 it's vacuously tight (1 failure suffices to evict). For K ≥ 2 it
guarantees ≥ K probeFail events between any Healthy and any subsequent Evicted.
-/

/-- Helper: when `run? sm events K` lands in `.evicted`, then either `sm`
    started in `.evicted`, or the events list contains at least one
    `probeFail` whose increment landed at or above K.

    More usefully for H5: the `failureCount` of a `.evicted` outcome is
    ≥ K.val.  -/
theorem evicted_failureCount_ge_K
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm')
    (hst : sm'.state = .evicted) :
    sm.state = .evicted ∨ sm'.failureCount ≥ K.val := by
  induction events generalizing sm with
  | nil =>
      simp [run?] at hrun
      left
      subst hrun
      exact hst
  | cons e rest ih =>
      simp [run?, List.foldl, Option.bind] at hrun
      cases hstep : step? sm e K with
      | none =>
          rw [hstep] at hrun
          simp at hrun
      | some sm'' =>
          rw [hstep] at hrun
          simp at hrun
          -- hrun : run? sm'' rest K = some sm'
          have := ih sm'' hrun
          cases this with
          | inl h =>
              -- sm''.state = .evicted; trace back through step?
              cases e <;> simp [step?] at hstep
              · -- probeSuccess: contradicts h
                rename_i b
                obtain rfl := Option.some_inj.mp hstep
                simp at h
                cases b <;> simp at h
              · -- probeFail
                split at hstep
                · -- ≥ K branch
                  rename_i hge
                  obtain rfl := Option.some_inj.mp hstep
                  right
                  -- sm''.failureCount = sm.failureCount + 1 ≥ K.val
                  -- now we need to track this through `run?` to sm'
                  -- the failureCount can change in rest events, so we instead
                  -- prove sm'.failureCount ≥ K.val by case-splitting rest
                  exact run_preserves_failureCount_or_resets sm'' sm' rest K hrun hst hge
                · simp at h
              · -- backoffExpiry
                obtain rfl := Option.some_inj.mp hstep
                cases sm.state <;> simp at h
              · -- registryAbsent ruled out by hstep
                contradiction
          | inr h => right; exact h

/-- Helper: across a `run?` that lands in `.evicted`, the failureCount is
    preserved or reset to 0 + re-incremented above K. Either way, the final
    `failureCount` is ≥ K.val. Used inside `evicted_failureCount_ge_K`. -/
theorem run_preserves_failureCount_or_resets
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm')
    (hst : sm'.state = .evicted)
    (hsm : sm.failureCount ≥ K.val) :
    sm'.failureCount ≥ K.val := by
  induction events generalizing sm with
  | nil =>
      simp [run?] at hrun
      subst hrun
      exact hsm
  | cons e rest ih =>
      simp [run?, List.foldl, Option.bind] at hrun
      cases hstep : step? sm e K with
      | none =>
          rw [hstep] at hrun
          simp at hrun
      | some sm'' =>
          rw [hstep] at hrun
          simp at hrun
          -- if sm''.state = .evicted with high count, recurse
          -- if sm''.state ≠ .evicted at this point, the next steps may
          -- transit through .evicted again with a fresh count
          cases e <;> simp [step?] at hstep
          · -- probeSuccess: resets count; sm''.state ≠ .evicted at this point
            -- but sm''.state could become .evicted later via failures
            -- Recurse: ih needs sm''.failureCount ≥ K.val, which is false here
            -- So we must instead use evicted_failureCount_ge_K on sm''/sm'/rest
            rename_i b
            obtain rfl := Option.some_inj.mp hstep
            have := evicted_failureCount_ge_K sm'' sm' rest K hrun hst
            cases this with
            | inl h => simp at h
              -- sm''.state = .evicted, but probeSuccess gives healthy/degraded
              cases b <;> simp at h
            | inr h => exact h
          · -- probeFail
            split at hstep
            · rename_i hge
              obtain rfl := Option.some_inj.mp hstep
              -- sm''.failureCount = sm.failureCount + 1 ≥ K.val ≥ K.val
              -- so we can use ih with the new hsm
              apply ih sm''
              · exact hrun
              · -- new hsm : sm''.failureCount ≥ K.val
                exact Nat.le_of_lt (Nat.lt_succ_of_le hge)
            · rename_i hlt
              -- sm''.failureCount = sm.failureCount + 1 < K.val
              -- contradicts hsm : sm.failureCount ≥ K.val ?? Not directly.
              -- sm.failureCount ≥ K.val means sm.failureCount + 1 ≥ K.val + 1 > K.val,
              -- which contradicts hlt.
              exfalso
              apply hlt
              omega
          · -- backoffExpiry: count unchanged
            obtain rfl := Option.some_inj.mp hstep
            apply ih
            · exact hrun
            · simp; exact hsm
          · -- registryAbsent
            contradiction

/-- H5 anti_flapping_inter_eviction_gap.

    If a run starting from `ServiceModel.initial` reaches `.healthy` at
    prefix `p1` (after `events.take p1`) and `.evicted` at later prefix `p2`
    (after `events.take p2`), then the slice `events[p1..p2]` contains at
    least K `.probeFail` events.

    This is the contract that catches the historical flapping bug class:
    at K=1, one fail is enough (matches today's Rust); at K ≥ 2, the model
    guarantees inter-eviction quiet windows.

    Proof strategy: at prefix p1 the state is `.healthy`, so failureCount is 0
    (probeSuccess resets to 0; the only way to be `.healthy` is via
    `probeSuccess(false)`). Between p1 and p2, the state lands in `.evicted`,
    so by `evicted_failureCount_ge_K`, the failureCount is ≥ K. Since each
    `probeFail` increments by exactly 1 and `probeSuccess` resets to 0, the
    count of `probeFail` events in the slice is ≥ K. -/
theorem h5_anti_flapping_inter_eviction_gap
    (events : List Event) (K : Threshold)
    (p1 p2 : Nat) (h12 : p1 < p2) (h2le : p2 ≤ events.length)
    (h1 : (run? ServiceModel.initial (events.take p1) K).map (·.state) = some .healthy)
    (h2 : (run? ServiceModel.initial (events.take p2) K).map (·.state) = some .evicted) :
    K.val ≤ ((events.drop p1).take (p2 - p1)).countP (· = Event.probeFail) := by
  -- Sketch (executor: fill in the tactic script):
  -- 1. From h1 extract sm1 with (run? init (events.take p1) K) = some sm1
  --    and sm1.state = .healthy, hence sm1.failureCount = 0 (by H2-style fact
  --    on the last event of the prefix being probeSuccess(false), or by initial).
  -- 2. From h2 extract sm2 similarly with sm2.state = .evicted.
  -- 3. Apply evicted_failureCount_ge_K to (sm1, sm2, events.drop p1 ⌢ … take (p2-p1))
  --    to get sm2.failureCount ≥ K.val.
  -- 4. Each .probeFail in the slice increments failureCount by 1; each
  --    .probeSuccess resets to 0; other events don't change failureCount.
  --    By the structure of step?, sm2.failureCount equals the count of
  --    .probeFail events since the last .probeSuccess in the slice
  --    (or since p1 if no probeSuccess in the slice).
  -- 5. count_eq_failureCount: the count of probeFail events bounded below
  --    by sm2.failureCount (any intervening probeSuccess would only INCREASE
  --    the required count to reach ≥ K).
  -- 6. Conclude.
  sorry
```

**IMPORTANT**: The `sorry` here is a stand-in only for this step's draft. **Step 2 of this task MUST replace it with a complete proof — no `sorry` remains in any committed file.**

- [ ] **Step 2: Complete the H5 proof.**

The proof has two pieces that need to come together:

**(a) `sm1.failureCount = 0` at the `.healthy` prefix.**

State a helper lemma:

```lean
theorem healthy_failureCount_eq_zero
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm')
    (hst : sm'.state = .healthy) :
    sm'.failureCount = 0 ∨ (events = [] ∧ sm.failureCount = 0 ∧ sm.state = .healthy) := by
  -- Induction on events.
  -- nil case: sm' = sm; if sm.state = .healthy then return the right disjunct.
  -- cons case: the only events that produce .healthy are
  --            probeSuccess(false) (which sets failureCount := 0) or
  --            backoffExpiry from a .healthy starting state (preserves count;
  --            but the prefix-induction means we can recurse to get count = 0).
  sorry
```

Drive the proof by case analysis on the last event of the prefix. `ServiceModel.initial.failureCount = 0` and `.state = .healthy` is the base case.

**(b) `sm2.failureCount` is a lower bound on the probeFail count in the slice.**

State the bridge lemma:

```lean
theorem failureCount_bounded_by_probefail_count
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm')
    (h0 : sm.failureCount = 0) :
    sm'.failureCount ≤ events.countP (· = Event.probeFail) := by
  induction events generalizing sm with
  | nil => simp [run?] at hrun; subst hrun; simp [h0]
  | cons e rest ih =>
      simp [run?, List.foldl, Option.bind] at hrun
      cases hstep : step? sm e K with
      | none => rw [hstep] at hrun; simp at hrun
      | some sm'' =>
          rw [hstep] at hrun; simp at hrun
          cases e <;> simp [step?] at hstep
          · -- probeSuccess: sm''.failureCount = 0; ih applies with h0 = 0
            rename_i b
            obtain rfl := Option.some_inj.mp hstep
            apply Nat.le_trans (ih sm'' hrun (by rfl))
            simp [List.countP_cons]
          · -- probeFail: sm''.failureCount = sm.failureCount + 1 = 1 (by h0)
            split at hstep
            · obtain rfl := Option.some_inj.mp hstep
              -- sm''.failureCount = 1; recurse with new h0 = 1
              -- But ih requires sm.failureCount = 0; here sm''.failureCount = 1
              -- So we need a strictly stronger ih ... or a different angle.
              sorry
            · obtain rfl := Option.some_inj.mp hstep
              sorry
          · -- backoffExpiry: failureCount preserved
            obtain rfl := Option.some_inj.mp hstep; simp
            apply Nat.le_trans (ih sm hrun h0)
            simp [List.countP_cons]
          · contradiction
```

**Sharper formulation** (replace the above with this):

```lean
/-- The bridge fact: across any `run?`, the gain in `failureCount` is bounded
    above by the count of `probeFail` events in the run. (Each probeFail
    increments by 1; probeSuccess resets to 0; backoffExpiry preserves.) -/
theorem failureCount_le_probefail_count
    (sm sm' : ServiceModel) (events : List Event) (K : Threshold)
    (hrun : run? sm events K = some sm') :
    sm'.failureCount ≤ sm.failureCount + events.countP (· = Event.probeFail) := by
  induction events generalizing sm with
  | nil =>
      simp [run?] at hrun
      subst hrun
      simp
  | cons e rest ih =>
      simp [run?, List.foldl, Option.bind] at hrun
      cases hstep : step? sm e K with
      | none => rw [hstep] at hrun; simp at hrun
      | some sm'' =>
          rw [hstep] at hrun; simp at hrun
          have hih := ih sm'' hrun
          cases e <;> simp [step?] at hstep
          · rename_i b
            obtain rfl := Option.some_inj.mp hstep
            -- sm''.failureCount = 0
            simp [List.countP_cons]
            omega
          · split at hstep
            · obtain rfl := Option.some_inj.mp hstep
              simp [List.countP_cons] at hih ⊢
              omega
            · obtain rfl := Option.some_inj.mp hstep
              simp [List.countP_cons] at hih ⊢
              omega
          · obtain rfl := Option.some_inj.mp hstep
            simp [List.countP_cons] at hih ⊢
            cases sm.state <;> simp at hih <;> omega
          · contradiction
```

Then H5 follows from `evicted_failureCount_ge_K` (sm2.failureCount ≥ K.val), `healthy_failureCount_eq_zero` (sm1.failureCount = 0), and `failureCount_le_probefail_count` applied to the slice between p1 and p2.

Final H5 proof:

```lean
theorem h5_anti_flapping_inter_eviction_gap
    (events : List Event) (K : Threshold)
    (p1 p2 : Nat) (h12 : p1 < p2) (h2le : p2 ≤ events.length)
    (h1 : (run? ServiceModel.initial (events.take p1) K).map (·.state) = some .healthy)
    (h2 : (run? ServiceModel.initial (events.take p2) K).map (·.state) = some .evicted) :
    K.val ≤ ((events.drop p1).take (p2 - p1)).countP (· = Event.probeFail) := by
  -- Extract sm1 from h1, sm2 from h2.
  obtain ⟨sm1, hsm1_run, hsm1_state⟩ := by
    rcases hrun1 : (run? ServiceModel.initial (events.take p1) K) with _ | sm1
    · simp [hrun1] at h1
    · simp [hrun1] at h1; exact ⟨sm1, hrun1, h1⟩
  obtain ⟨sm2, hsm2_run, hsm2_state⟩ := by
    rcases hrun2 : (run? ServiceModel.initial (events.take p2) K) with _ | sm2
    · simp [hrun2] at h2
    · simp [hrun2] at h2; exact ⟨sm2, hrun2, h2⟩
  -- Bridge: run? sm1 (slice) K = some sm2, where slice = events[p1..p2].
  have hslice : run? sm1 ((events.drop p1).take (p2 - p1)) K = some sm2 := by
    -- run? sm0 (a ++ b) K = (run? sm0 a K).bind (run? · b K) — split events.take p2.
    -- events.take p2 = events.take p1 ++ (events.drop p1).take (p2 - p1).
    sorry  -- executor: prove via List.take_append_drop / run? composition lemma.
  -- sm1.failureCount = 0 because sm1.state = .healthy.
  have hsm1_fc : sm1.failureCount = 0 := by
    have := healthy_failureCount_eq_zero ServiceModel.initial sm1
      (events.take p1) K hsm1_run hsm1_state
    cases this with
    | inl h => exact h
    | inr h => obtain ⟨_, h_init_fc, _⟩ := h; exact h_init_fc
  -- sm2.failureCount ≥ K.val because sm2.state = .evicted and sm1.state ≠ .evicted.
  have hsm2_fc : sm2.failureCount ≥ K.val := by
    have := evicted_failureCount_ge_K sm1 sm2 _ K hslice hsm2_state
    cases this with
    | inl h => exfalso; rw [h] at hsm1_state; cases hsm1_state
    | inr h => exact h
  -- Apply the bound: sm2.failureCount ≤ sm1.failureCount + slice.probeFail_count.
  have := failureCount_le_probefail_count sm1 sm2 _ K hslice
  omega
```

**Need: `run?` composition lemma.** Add it to `Transition.lean` (or as a private lemma in `Properties.lean`):

```lean
/-- Compose two runs: `run? sm (a ++ b) K = (run? sm a K).bind (run? · b K)`. -/
theorem run?_append (sm : ServiceModel) (a b : List Event) (K : Threshold) :
    run? sm (a ++ b) K = (run? sm a K).bind (fun sm' => run? sm' b K) := by
  induction a generalizing sm with
  | nil => simp [run?]
  | cons e rest ih =>
      simp [run?, List.foldl, Option.bind]
      cases step? sm e K with
      | none => simp
      | some sm' => simp; exact ih sm'
```

Add `run?_append` to `Transition.lean` after the existing `run?_singleton` lemma, then use it in the H5 proof: `events.take p2 = events.take p1 ++ (events.drop p1).take (p2 - p1)` (standard `List` lemma `List.take_append_take_drop` or hand-rolled with `List.take_append_drop` and arithmetic on the lengths).

- [ ] **Step 3: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build, **zero `sorry`**. If H5's proof has a residual `sorry`, the task is not complete — fix it before committing.

If proof tactics need adjustment to compile under the actual Lean toolchain, the mathematical content is what's load-bearing: H5 follows from (i) the run lands in `.evicted` with `failureCount ≥ K`, (ii) it started in `.healthy` with `failureCount = 0`, (iii) every probeFail increments by ≤ 1 and probeSuccess resets to 0. Adjust tactics until `lake build` is clean.

- [ ] **Step 4: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/Properties.lean \
        crates/defra-agent/proofs/Proofs/MCPHealth/Transition.lean
git commit -m "Proofs.MCPHealth: prove H5 anti-flapping inter-eviction gap (#186)"
```

---

## Task 8: Coupling.lean — healthProjection + C1–C4

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/MCPHealth/Coupling.lean`
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth.lean` (uncomment Coupling import)

- [ ] **Step 1: Create `Coupling.lean`.**

```lean
import Proofs.MCPHealth.State
import Proofs.ToolExecution.Policy

/-!
# MCP Health — Coupling to ToolExecution.Policy

Projects the four-state `HealthState` down to the three-value
`ToolExecution.Health` ADT that `preflight` already keys on. Then proves
the four bridging lemmas (C1–C4):

* Evicted / Reconnecting block dispatch as ServiceUnavailable.
* Healthy / Degraded dispatch (when schema is valid or unchecked).

This file is the **only** file in `Proofs.MCPHealth` that imports
`Proofs.ToolExecution.Policy`. It does not modify `Policy.lean` — purely
additive extension. See spec §5 for the rationale (preflight is the
correct coupling axis, not `retryDisposition`).
-/

namespace Proofs.MCPHealth

/-- Project the four-state lifecycle to the three-value Health ADT.

    Both flavors of `.degraded` (staleness-degraded and
    failure-count-degraded) project to `.stale`, reflecting that both admit
    dispatch with a longer timeout. -/
def healthProjection : HealthState → ToolExecution.Health
  | .healthy      => .healthy
  | .degraded     => .stale
  | .evicted      => .unreachable
  | .reconnecting => .unreachable

namespace Coupling

/-- C1: Evicted services block dispatch as ServiceUnavailable. -/
theorem c1_evicted_blocks_dispatch (schema : ToolExecution.SchemaStatus) :
    ToolExecution.preflight (healthProjection .evicted) schema
      = .block .serviceUnavailable := by
  cases schema <;> rfl

/-- C2: Reconnecting services block dispatch as ServiceUnavailable. -/
theorem c2_reconnecting_blocks_dispatch (schema : ToolExecution.SchemaStatus) :
    ToolExecution.preflight (healthProjection .reconnecting) schema
      = .block .serviceUnavailable := by
  cases schema <;> rfl

/-- C3: Healthy services with valid (or unchecked) schema dispatch. -/
theorem c3_healthy_dispatches
    (schema : ToolExecution.SchemaStatus) (hv : schema ≠ .invalid) :
    ToolExecution.preflight (healthProjection .healthy) schema = .dispatch := by
  cases schema
  · rfl
  · rfl
  · exact absurd rfl hv

/-- C4: Degraded services dispatch (matches today's "stale services allowed
    through with a longer timeout" behavior). -/
theorem c4_degraded_dispatches
    (schema : ToolExecution.SchemaStatus) (hv : schema ≠ .invalid) :
    ToolExecution.preflight (healthProjection .degraded) schema = .dispatch := by
  cases schema
  · rfl
  · rfl
  · exact absurd rfl hv

end Coupling
end Proofs.MCPHealth
```

- [ ] **Step 2: Uncomment the Coupling import in `MCPHealth.lean`.**

```lean
import Proofs.MCPHealth.State
import Proofs.MCPHealth.Transition
import Proofs.MCPHealth.Properties
import Proofs.MCPHealth.Coupling

-- Subsequent imports added as tasks land:
-- import Proofs.MCPHealth.Executable
```

- [ ] **Step 3: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build, no `sorry`.

- [ ] **Step 4: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/Coupling.lean \
        crates/defra-agent/proofs/Proofs/MCPHealth.lean
git commit -m "Proofs.MCPHealth: project HealthState to ToolExecution.Health (#186)"
```

---

## Task 9: Executable.lean — TransitionCase + transitionCases

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/MCPHealth/Executable.lean`
- Modify: `crates/defra-agent/proofs/Proofs/MCPHealth.lean` (uncomment Executable import)

- [ ] **Step 1: Create `Executable.lean`.**

```lean
import Proofs.MCPHealth.Transition
import Proofs.MCPHealth.Coupling

/-!
# MCP Health — Conformance witnesses

Exhaustive enumeration of transitions across
`K ∈ {1, 2, 3} × startState ∈ HealthState.all × startCount ∈ {0..K} ×
event ∈ Event.all`. Each row is evaluated by `step?` and tagged with the
Rust 3-state projection.

`k1ProjectionCases` is the subset Rust consumes today (K=1, matching the
current `health_checker.rs` behavior). The K ≥ 2 rows are emitted but not
yet asserted by any Rust test — they form the formal contract for a future
K-aware refactor.
-/

namespace Proofs.MCPHealth

/-- Witness row for a single transition. -/
structure TransitionCase where
  name           : String
  startState     : HealthState
  startCount     : Nat
  event          : Event
  thresholdK     : Nat
  nextState      : Option HealthState   -- none = service removed
  nextCount      : Option Nat
  rustProjection : Option String         -- "healthy" | "stale" | "unreachable" | none
  deriving Repr

namespace TransitionCase

/-- Build a row by applying `step?` to (startState, startCount, event, K). -/
def build (startState : HealthState) (startCount : Nat) (event : Event)
    (thresholdK : Nat) (hk : thresholdK ≥ 1) : TransitionCase :=
  let K : Threshold := Threshold.ofNat thresholdK hk
  let sm : ServiceModel := { state := startState, failureCount := startCount }
  let next := step? sm event K
  let nameSuffix := match next with
    | none => "removed"
    | some sm' => sm'.state.toDefraDB ++ "_" ++ toString sm'.failureCount
  { name :=
      "mcp_health_K" ++ toString thresholdK ++ "_"
        ++ startState.toDefraDB ++ "_" ++ toString startCount ++ "_"
        ++ event.toDefraDB ++ "_" ++ nameSuffix
  , startState := startState
  , startCount := startCount
  , event := event
  , thresholdK := thresholdK
  , nextState := next.map (·.state)
  , nextCount := next.map (·.failureCount)
  , rustProjection :=
      next.map fun sm' => (healthProjection sm'.state).toDefraDB
  }

end TransitionCase

/-- Range `[0..K]` of valid starting `failureCount` values for a given K.
    `0` for staleness-degraded / Healthy / Reconnecting; up to `K-1` for
    failure-count-degraded; `K` only appears as a *next* count, not a start. -/
def countRange (K : Nat) : List Nat :=
  (List.range K)  -- [0, 1, ..., K-1]

/-- Generate all rows for a single K. -/
def transitionCasesFor (K : Nat) (hk : K ≥ 1) : List TransitionCase :=
  HealthState.all.flatMap fun s =>
    (countRange K).flatMap fun n =>
      Event.all.map fun e =>
        TransitionCase.build s n e K hk

/-- All conformance rows for K ∈ {1, 2, 3}. -/
def transitionCases : List TransitionCase :=
  transitionCasesFor 1 (by decide) ++
  transitionCasesFor 2 (by decide) ++
  transitionCasesFor 3 (by decide)

/-- K=1 subset — the rows Rust witnesses today. -/
def k1ProjectionCases : List TransitionCase :=
  transitionCases.filter (·.thresholdK = 1)

/-- K ≥ 2 subset — emitted but not yet asserted by any Rust test. -/
def k2PlusFutureCases : List TransitionCase :=
  transitionCases.filter (·.thresholdK ≥ 2)

/-- `k1ProjectionCases` and `k2PlusFutureCases` partition `transitionCases`. -/
theorem k1_k2_partition :
    k1ProjectionCases.length + k2PlusFutureCases.length = transitionCases.length := by
  simp [k1ProjectionCases, k2PlusFutureCases, transitionCases,
        transitionCasesFor, List.filter, List.length]
  decide

end Proofs.MCPHealth
```

- [ ] **Step 2: Uncomment the Executable import in `MCPHealth.lean`.**

```lean
import Proofs.MCPHealth.State
import Proofs.MCPHealth.Transition
import Proofs.MCPHealth.Properties
import Proofs.MCPHealth.Coupling
import Proofs.MCPHealth.Executable
```

- [ ] **Step 3: Verify `lake build`.**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: clean build, no `sorry`.

If the `decide` tactic times out or fails on `k1_k2_partition`, replace with `by rfl` or `by simp [...]; norm_num` — the proof is purely computational over a finite list.

- [ ] **Step 4: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/MCPHealth/Executable.lean \
        crates/defra-agent/proofs/Proofs/MCPHealth.lean
git commit -m "Proofs.MCPHealth: emit conformance transition cases (K=1, K=2, K=3) (#186)"
```

---

## Task 10: Wire conformance JSON emission

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`

The Lean→Rust bridge runs `lake env lean --run Proofs/Conformance/Contracts.lean` and parses the JSON between sentinel markers. We add one new field `"mcp_health_cases"` to the top-level JSON object.

- [ ] **Step 1: Add the import for the new module to `Json.lean`.**

Read `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean` and locate the import section near the top of the file. Add:

```lean
import Proofs.MCPHealth.Executable
```

next to the existing `Proofs.ToolExecution.Policy` (or near it).

- [ ] **Step 2: Add the per-row emitter helper in `Json.lean`.**

After `toolRetryCaseJson` (around line 235 in the file, locate by `grep -n "toolRetryCaseJson" crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`), insert:

```lean
def mcpHealthCaseJson (witness : Proofs.MCPHealth.TransitionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"start_state\":" ++ jsonString witness.startState.toDefraDB ++ ","
    ++ "\"start_count\":" ++ toString witness.startCount ++ ","
    ++ "\"event\":" ++ jsonString witness.event.toDefraDB ++ ","
    ++ "\"threshold_k\":" ++ toString witness.thresholdK ++ ","
    ++ "\"next_state\":"
      ++ jsonOptionalString (witness.nextState.map Proofs.MCPHealth.HealthState.toDefraDB) ++ ","
    ++ "\"next_count\":"
      ++ (match witness.nextCount with
          | none => "null"
          | some n => toString n) ++ ","
    ++ "\"rust_projection\":"
      ++ jsonOptionalString witness.rustProjection
    ++ "}"
```

(If `jsonOptionalString` is the helper already used for similar nullable strings, reuse it. Otherwise look up the existing helper in `Json.lean` and use whichever matches.)

- [ ] **Step 3: Wire the new field into the top-level JSON emitter.**

Find the closing brace of the top-level JSON (around line 427: `++ "}"`). Insert the new field **immediately before** `++ "\"follow_up_hooks\":[],"`:

```lean
    ++ "\"mcp_health_cases\":"
      ++ jsonArray (Proofs.MCPHealth.transitionCases.map mcpHealthCaseJson) ++ ","
    ++ "\"follow_up_hooks\":[],"
```

- [ ] **Step 4: Add a coverage-ledger entry in `CoverageLedger.lean`.**

Locate the `consumerCoverage` block near line 105 (after the `ToolRetryDisposition` entry). Insert:

```lean
  , consumerCoverage
      "state_machine"
      "MCPHealth"
      "health_checker::tests::generated_mcp_health_k1_cases_match_health_checker_transitions"
```

The placement is between the existing entries; choose the spot that keeps the existing ordering pattern (state_machine entries are typically grouped — find a nearby state-machine row and insert next to it). If unclear, place it at the end of the `consumerCoverage` list, just before the closing `]`.

- [ ] **Step 5: Verify `lake build` and JSON emission.**

```bash
cd crates/defra-agent/proofs && lake build
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean | grep -A1 mcp_health_cases | head -10
```

Expected: build is clean; the second command prints a JSON snippet showing `"mcp_health_cases":[ ... rows ...`.

- [ ] **Step 6: Commit.**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean \
        crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
git commit -m "Conformance: emit mcp_health_cases JSON; register coverage row (#186)"
```

---

## Task 11: Wire LeanContractSnapshot + LeanMcpHealthCase

**Files:**
- Modify: `crates/defra-agent/src/lean_vocab_test.rs`

- [ ] **Step 1: Add `LeanMcpHealthCase` struct.**

In `crates/defra-agent/src/lean_vocab_test.rs`, after `LeanToolRetryCase` (around line 440, locate via `grep -n "struct LeanToolRetryCase" crates/defra-agent/src/lean_vocab_test.rs`), insert:

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct LeanMcpHealthCase {
    pub(crate) name: String,
    pub(crate) start_state: String,
    pub(crate) start_count: usize,
    pub(crate) event: String,
    pub(crate) threshold_k: usize,
    pub(crate) next_state: Option<String>,
    pub(crate) next_count: Option<usize>,
    pub(crate) rust_projection: Option<String>,
}
```

- [ ] **Step 2: Add the field to `LeanContractSnapshot`.**

Locate the `LeanContractSnapshot` struct (around line 27). Insert a new field **next to `tool_retry_cases`** (so related test surfaces stay grouped):

```rust
pub(crate) tool_retry_cases: Vec<LeanToolRetryCase>,
pub(crate) mcp_health_cases: Vec<LeanMcpHealthCase>,
```

- [ ] **Step 3: Add the accessor functions.**

After `lean_tool_retry_case` (around line 690, locate via `grep -n "lean_tool_retry_case" crates/defra-agent/src/lean_vocab_test.rs`), insert:

```rust
pub(crate) fn lean_mcp_health_cases() -> &'static [LeanMcpHealthCase] {
    &lean_contract_snapshot().mcp_health_cases
}

pub(crate) fn lean_mcp_health_k1_cases() -> Vec<&'static LeanMcpHealthCase> {
    lean_contract_snapshot()
        .mcp_health_cases
        .iter()
        .filter(|case| case.threshold_k == 1)
        .collect()
}
```

- [ ] **Step 4: Verify `cargo check` (test target).**

```bash
cargo check -p defra-agent --tests
```

Expected: no errors. The new struct and field should parse cleanly. Warnings about unused functions are fine — the Rust test in Task 12 will consume them.

- [ ] **Step 5: Commit.**

```bash
git add crates/defra-agent/src/lean_vocab_test.rs
git commit -m "lean_vocab_test: wire LeanMcpHealthCase + accessors (#186)"
```

---

## Task 12: Add the Rust consumer test in health_checker.rs

**Files:**
- Modify: `crates/defra-agent/src/health_checker.rs`

The new test consumes the K=1 rows from Lean and asserts the projection matches today's Rust behavior. **No changes to `run_health_check` or any production code.**

- [ ] **Step 1: Write the failing test first.**

In `crates/defra-agent/src/health_checker.rs`, locate the existing test module (`#[cfg(test)] mod registry_parsing_tests`, around line 324). Add a **new test module** below it (sibling, not nested):

```rust
#[cfg(test)]
mod transitions_tests {
    use super::HealthStatus;
    use crate::lean_vocab_test::{lean_mcp_health_k1_cases, LeanMcpHealthCase};

    /// Project a Lean K=1 case's `rust_projection` to a `HealthStatus` for
    /// today's Rust to assert against. `None` means the service was removed
    /// via `registryAbsent`; tests skip those rows (Rust represents removal
    /// by dropping the map entry, not by a `HealthStatus`).
    fn projected_health_status(case: &LeanMcpHealthCase) -> Option<HealthStatus> {
        case.rust_projection.as_deref().map(|s| match s {
            "healthy" => HealthStatus::Healthy,
            "stale" => HealthStatus::Stale,
            "unreachable" => HealthStatus::Unreachable,
            other => panic!(
                "Lean MCP health case {} produced unknown rust_projection {:?}",
                case.name, other
            ),
        })
    }

    /// Simulate one tick of `run_health_check`'s decision logic for a given
    /// case: given a starting `HealthStatus` and a probe event, what does
    /// today's Rust assign?
    ///
    /// This mirrors the inline logic at `health_checker.rs:247–308` but
    /// drives it with the case's event rather than a real probe call.
    fn rust_simulated_next(case: &LeanMcpHealthCase) -> Option<HealthStatus> {
        match case.event.as_str() {
            "registryAbsent" => None,
            "backoffExpiry" => {
                // Today's Rust has no backoffExpiry behavior — backoff is not
                // armed at K=1. Express this by mapping the projection back
                // (no observable change at K=1).
                Some(start_status(case))
            }
            "probeSuccessFresh" => Some(HealthStatus::Healthy),
            "probeSuccessStale" => Some(HealthStatus::Stale),
            "probeFail" => Some(HealthStatus::Unreachable),
            other => panic!(
                "Lean MCP health case {} produced unknown event {:?}",
                case.name, other
            ),
        }
    }

    fn start_status(case: &LeanMcpHealthCase) -> HealthStatus {
        match case.start_state.as_str() {
            "healthy" => HealthStatus::Healthy,
            "degraded" => HealthStatus::Stale, // K=1: degraded only via staleness
            "evicted" | "reconnecting" => HealthStatus::Unreachable,
            other => panic!(
                "Lean MCP health case {} produced unknown start_state {:?}",
                case.name, other
            ),
        }
    }

    #[test]
    fn generated_mcp_health_k1_cases_match_health_checker_transitions() {
        let cases = lean_mcp_health_k1_cases();
        assert!(
            !cases.is_empty(),
            "Lean must emit at least one K=1 MCP health case"
        );

        for case in cases {
            let expected = projected_health_status(case);
            let actual = rust_simulated_next(case);
            assert_eq!(
                actual, expected,
                "Lean MCP health K=1 case {} must match Rust HealthStatus assignment",
                case.name
            );
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it compiles and either passes or fails meaningfully.**

```bash
cargo test -p defra-agent transitions_tests::generated_mcp_health_k1_cases_match_health_checker_transitions -- --nocapture
```

Expected: passes. The K=1 conformance rows from Lean (`Healthy + probeFail → Evicted` projects to `Unreachable`; `Healthy + probeSuccess(true) → Degraded` projects to `Stale`; etc.) should all match `rust_simulated_next`.

If it fails: the most likely cause is a row-by-row mismatch between Lean's `healthProjection` table and the Rust simulation logic. Read the assertion failure message — it names the offending case — and reconcile. Either Lean's emission is wrong (re-check `healthProjection` in `Coupling.lean`) or Rust's `rust_simulated_next` doesn't capture today's behavior accurately.

- [ ] **Step 3: Commit.**

```bash
git add crates/defra-agent/src/health_checker.rs
git commit -m "health_checker: consume Lean K=1 MCP health conformance rows (#186)"
```

---

## Task 13: Final verification — full test + lake build

**Files:** (no edits in this task; verification only)

- [ ] **Step 1: Run the full Lean build.**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: clean build, zero `sorry`, no warnings.

Verify no `sorry` anywhere in the new module:

```bash
grep -rn "sorry" crates/defra-agent/proofs/Proofs/MCPHealth/
```

Expected: no matches (or only matches inside string literals / doc comments — re-read each line; the doc strings in `Task 5/7` should not contain the word `sorry` as a placeholder. Replace any residual `sorry` placeholders with `by ...` proofs before continuing.)

- [ ] **Step 2: Run the full Rust test suite for the agent crate.**

```bash
cargo test -p defra-agent
```

Expected: all existing tests pass; the new test `transitions_tests::generated_mcp_health_k1_cases_match_health_checker_transitions` passes.

If any unrelated test fails: investigate the root cause; don't disable or skip.

- [ ] **Step 3: Verify the conformance JSON sentinel block parses end-to-end.**

```bash
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean | \
    grep -c '"mcp_health_cases"'
```

Expected: `1` (the field appears exactly once in the JSON).

- [ ] **Step 4: Sanity-check the K=1 row count.**

```bash
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean | \
    grep -o '"threshold_k":1' | wc -l
```

Expected: the count equals `|HealthState.all| * K=1's countRange * |Event.all| = 4 * 1 * 5 = 20` rows at K=1. (`countRange 1 = [0]` is a one-element list.)

- [ ] **Step 5: Confirm no edits to `Proofs/ToolExecution/Policy.lean` or production Rust.**

```bash
git diff main -- crates/defra-agent/proofs/Proofs/ToolExecution/Policy.lean
git diff main -- crates/defra-agent/src/health_checker.rs | grep -v "^+#\[cfg(test)\]\|^+mod transitions_tests\|^+    \|^+}" | head
```

Expected: first command shows no diff. Second command's filtered output (anything outside the `#[cfg(test)]` block) is empty.

If either shows unexpected diffs, revert those changes — production code should be untouched.

- [ ] **Step 6: No new commit in this task (verification only). Move to Task 14 to open the PR.**

---

## Task 14: Open the pull request

**Files:** (no file edits; PR creation only)

- [ ] **Step 1: Confirm branch state.**

```bash
git status
git log --oneline main..HEAD
```

Expected: clean tree (no uncommitted changes); 8–11 commits ahead of main (one per task), all signed by the implementer.

- [ ] **Step 2: Push the branch.**

```bash
git push -u origin proofs/issue-186-mcp-health-eviction
```

- [ ] **Step 3: Open the PR.**

```bash
gh pr create --title "Add MCP health probe / eviction state machine in Lean" --body "$(cat <<'EOF'
## Summary

Adds a per-service Lean state machine over the four-state lifecycle
`healthy → degraded → evicted → reconnecting`, parameterized by a failure-count
threshold K. K=1 collapses to today's Rust single-probe-failure eviction;
K≥2 admits the bounded-flap regime.

## States and transitions

- `Healthy → Degraded` on `probeSuccess(staleness=true)` (any K) or
  `probeFail` when `failureCount + 1 < K` (K≥2 only).
- `Degraded → Evicted` on `probeFail` when `failureCount + 1 ≥ K`.
- `Evicted → Reconnecting` on `backoffExpiry`.
- `Reconnecting → Healthy` on `probeSuccess(false)`.
- Permissive direct path: `Evicted → Healthy` on `probeSuccess(false)`
  (the `Reconnecting` pass-through is optional; this matches today's K=1 Rust
  where no observable `Reconnecting` state exists).

## Load-bearing safety property

**H5 anti-flapping inter-eviction gap.** If a run reaches `.healthy` at prefix
`p1` and `.evicted` at later prefix `p2`, the event slice `events[p1..p2]`
contains ≥ K `.probeFail` events. At K=1 this admits today's single-failure
eviction; at K≥2 it guarantees inter-eviction quiet windows — the contract
that closes the flapping connection-pool bug class the audit identified.

## Conformance vectors registered

- `Proofs.MCPHealth.transitionCases` — exhaustive over
  `K ∈ {1,2,3} × startState × startCount ∈ [0..K) × event`.
- `Proofs.MCPHealth.k1ProjectionCases` — K=1 subset, consumed by the new
  Rust test `health_checker::transitions_tests::generated_mcp_health_k1_cases_match_health_checker_transitions`.
- `Proofs.MCPHealth.k2PlusFutureCases` — K≥2 subset, emitted as the formal
  contract for a future K-aware Rust refactor; not asserted by any Rust test
  yet (deliberately).

## Coupling to `ToolRetryDisposition`

Reframed at design time (see `docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md` §5).
The load-bearing coupling is to **`preflight`**, not `retryDisposition`:
`retryDisposition` is keyed on operation/idempotency/failure-class and
adding `HealthState` would break that orthogonality (and conflict with the
"additive only" rule for `Proofs/ToolExecution/Policy.lean`). Instead,
`Proofs/MCPHealth/Coupling.lean` defines `healthProjection : HealthState →
ToolExecution.Health` and proves four lemmas (C1–C4) that compose the
projection with the existing `preflight` contract. `Policy.lean` is not
modified.

## Scope discipline

- Zero `sorry`.
- No Rust production code changes (no edits to `HealthStatus`, `ServiceHealth`,
  `run_health_check`, or `mcp_pool.rs`).
- No TLA+ artifact (cross-node coordination is not in scope).
- No edits to `Proofs/ToolExecution/Policy.lean` (additive extension only).
- One new line in `Proofs.lean`.

## Test plan

- [ ] `cd crates/defra-agent/proofs && lake build` is clean, zero `sorry`.
- [ ] `cargo test -p defra-agent` passes; new test
      `transitions_tests::generated_mcp_health_k1_cases_match_health_checker_transitions`
      is among the passing tests.
- [ ] `lake env lean --run Proofs/Conformance/Contracts.lean` emits a
      `mcp_health_cases` field with > 0 rows.
- [ ] No diff to `Proofs/ToolExecution/Policy.lean`.
- [ ] No production-code diff in `health_checker.rs`/`mcp_pool.rs` (only
      `#[cfg(test)]` additions).

Closes #186
Refs #183

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR opens; `gh` returns the PR URL.

- [ ] **Step 4: Report the PR URL back to the user.**

---

## Self-review notes (for the executor)

Before the final PR, re-verify against the spec (`docs/superpowers/specs/2026-05-13-mcp-health-lean-design.md`):

- **§3 acceptance row coverage:** every row in the spec's acceptance table should map to a task above. Spot-check: H5 → Task 7; H6/H6' → Task 6; H7 → Task 5; H1/H2/H3/H4/H8 → Task 4; C1–C4 → Task 8; conformance vectors → Task 9; Rust consumer → Task 12; CoverageLedger → Task 10.
- **§7.3 K=1 table:** every row in the spec's K=1 collapse table should appear as a row in `k1ProjectionCases`. Sanity-check by row count (expected: 20 rows).
- **§14 risks:** all named risks (permissive recovery, backoff minimality, registry-absent terminal, staleness as observation) are acknowledged in code comments. The permissive-recovery risk specifically is addressed by H6' (Task 6).
- **§12 constraints:** zero `sorry`, no production-Rust changes, no edits to `Policy.lean`, named `Proofs.lean` import — all enforced by Task 13's verification steps.

If any spec requirement maps to no task, add the task before pushing.
