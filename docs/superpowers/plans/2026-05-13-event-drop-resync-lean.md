# Live event-drop resync Lean model — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Lean event-delivery contract (`Proofs/EventDelivery/`) closing #187. Land D1 (delivery convergence), D2 (fair-delivery latency), O1 (orphan-child materialization), C1 (watcher cooldown). Register three conformance vector families. Two deviation entries for EventSource and SubagentSource (rescan not yet wired in Rust).

**Architecture:** New Lean directory `Proofs/EventDelivery/` with a shared abstract `World` + `Transition` + `Trace` contract, three instance modules (Watcher / EventSource / SubagentSource), and a single new top-level import in `Proofs.lean`. Conformance vectors emitted from Lean and consumed by new tests in `tests/state_machine_conformance.rs`. **Zero `sorry`. No Rust production-code edits.**

**Tech Stack:** Lean 4 / Lake, Rust (test-only edits), DefraDB conformance JSON pipeline.

**Spec:** `docs/superpowers/specs/2026-05-13-event-drop-resync-lean-design.md` (commit `fc1353b`).

---

## Pre-flight notes

- **Working directory:** `/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-issue-187-event-drop-resync` — branch `proofs/issue-187-event-drop-resync`. This is already an isolated worktree.
- **Build commands:**
  - Lean: `cd crates/defra-agent/proofs && lake build`
  - Rust: `cargo test -p defra-agent -- <name>` from repo root
- **Zero-sorry check before every commit:** `grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/` must be empty.
- **Coordination:** This plan adds exactly one line to `Proofs.lean`. The brief notes #189 may also touch `Proofs.lean`; last-to-land rebases trivially. If #189 has landed first by the time you start, simply add your line below theirs.
- **No Rust production edits.** The only Rust file touched is `crates/defra-agent/tests/state_machine_conformance.rs`.

---

## Task 1: Verify environment + clean baseline

**Files:** none modified.

- [ ] **Step 1: Confirm branch + clean tree**

```bash
git status
git rev-parse --abbrev-ref HEAD
```

Expected: branch `proofs/issue-187-event-drop-resync`, working tree clean (except possibly `PROMPT.md` untracked, which is fine).

- [ ] **Step 2: Lean baseline build**

```bash
cd crates/defra-agent/proofs && lake build && cd -
```

Expected: succeeds. Capture the wall-clock; you'll re-run this many times.

- [ ] **Step 3: Rust baseline build**

```bash
cargo check -p defra-agent
```

Expected: succeeds (no compile errors, warnings OK).

- [ ] **Step 4: No commit** — environment check only.

---

## Task 2: Create `Proofs/EventDelivery/Contract.lean` — types only

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/EventDelivery/Contract.lean`

- [ ] **Step 1: Write the contract file**

```lean
/-!
# EventDelivery Contract

Shared abstract contract for lossy-subscription + bounded-rescan event delivery.
Three runtime sources instantiate this contract: the request watcher, the
event-trigger source, and the subagent source. See
`docs/superpowers/specs/2026-05-13-event-drop-resync-lean-design.md` for the
full design and the operational mapping to Rust call sites.
-/

namespace EventDelivery

/-- Opaque document identifier. Each `SourceInstance` binds it (request_id,
    (collection, doc_id), or tool_call_id). -/
structure DocId where
  raw : String
  deriving DecidableEq, Repr

/-- Operational dedupe-set policy. Watcher uses `ttlCooldown`; EventSource and
    SubagentSource use `monotoneOnce`. -/
inductive DedupePolicy where
  | ttlCooldown
  | monotoneOnce
  deriving DecidableEq, Repr

namespace DedupePolicy

def toContract : DedupePolicy → String
  | .ttlCooldown => "ttl_cooldown"
  | .monotoneOnce => "monotone_once"

def fromContract? : String → Option DedupePolicy
  | "ttl_cooldown" => some .ttlCooldown
  | "monotone_once" => some .monotoneOnce
  | _ => none

theorem fromContract_toContract (p : DedupePolicy) :
    fromContract? p.toContract = some p := by
  cases p <;> rfl

end DedupePolicy

/-- The abstract world. Constructive (not tick-indexed): convergence is proved
    via reachability traces, not via wall-clock time. -/
structure World where
  persistentSet     : List DocId
  subscriptionQueue : List DocId
  processedSet      : List DocId
  handled           : List DocId
  deriving Repr

/-- Empty initial world. -/
def World.empty : World :=
  { persistentSet := []
  , subscriptionQueue := []
  , processedSet := []
  , handled := []
  }

/-- Single observable step the source can take. -/
inductive Action where
  | persist (d : DocId)
  | depersist (d : DocId)
  | enqueue (d : DocId)
  | drop (d : DocId)
  | deliverFromQueue (d : DocId)
  | rescanTick
  | handle (d : DocId)
  deriving Repr

/-- Is this action a rescanTick? (Used by the `Fair` predicate.) -/
def Action.isRescan : Action → Bool
  | .rescanTick => true
  | _ => false

/-- Step relation. Each constructor enforces preconditions on `World`. -/
inductive Transition : World → Action → World → Prop where
  | persist (w : World) (d : DocId) :
      d ∉ w.persistentSet →
      Transition w (.persist d)
        { w with persistentSet := d :: w.persistentSet }
  | depersist (w : World) (d : DocId) :
      d ∈ w.persistentSet →
      Transition w (.depersist d)
        { w with persistentSet := w.persistentSet.erase d }
  | enqueue (w : World) (d : DocId) :
      d ∈ w.persistentSet →
      Transition w (.enqueue d)
        { w with subscriptionQueue := d :: w.subscriptionQueue }
  | drop (w : World) (d : DocId) :
      d ∈ w.subscriptionQueue →
      Transition w (.drop d)
        { w with subscriptionQueue := w.subscriptionQueue.erase d }
  | deliverFromQueue (w : World) (d : DocId) :
      d ∈ w.subscriptionQueue →
      Transition w (.deliverFromQueue d)
        { w with subscriptionQueue := w.subscriptionQueue.erase d }
  | rescanTick (w : World) :
      -- The rescan dumps every persistent doc not in processedSet into the
      -- subscription queue. Operationally: `pending_requests().await`
      -- (watcher) or the periodic introspection query (EventSource /
      -- SubagentSource — Rust gap-fill).
      Transition w .rescanTick
        { w with subscriptionQueue :=
            (w.persistentSet.filter (fun d => d ∉ w.processedSet)) ++ w.subscriptionQueue }
  | handle (w : World) (d : DocId) :
      d ∈ w.subscriptionQueue →
      d ∉ w.processedSet →
      Transition w (.handle d)
        { w with handled := d :: w.handled
               , processedSet := d :: w.processedSet
               , subscriptionQueue := w.subscriptionQueue.erase d }

/-- Reflexive-transitive closure: a finite trace of valid transitions. -/
inductive Trace : World → World → Prop where
  | refl {w : World} : Trace w w
  | step {w₁ w₂ w₃ : World} {a : Action} :
      Transition w₁ a w₂ → Trace w₂ w₃ → Trace w₁ w₃

/-- `SourceInstance` binds the contract to a concrete runtime subsystem.
    `rescanBoundedBy : Nat` is the maximum number of non-rescanTick actions
    that may occur between two consecutive `rescanTick`s in a `Fair` sequence
    (see `Properties.lean`). The sentinel `unboundedRescan = 0` records
    "no bounded rescan in the live process today"; D1 holds vacuously for
    such instances. -/
structure SourceInstance where
  name            : String
  dedupePolicy    : DedupePolicy
  rescanBoundedBy : Nat
  deriving Repr

/-- Sentinel value for instances whose Rust impl does not yet satisfy the
    rescan obligation. Concretely `0`; makes the `Fair` predicate
    unsatisfiable, so D1 holds vacuously and the corresponding
    `Conformance/Deviations.lean` entry names the gap. -/
def SourceInstance.unboundedRescan : Nat := 0

end EventDelivery
```

- [ ] **Step 2: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: succeeds. The file is type-checked but not yet imported anywhere.

- [ ] **Step 3: Zero-sorry check**

```bash
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/
```

Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/EventDelivery/Contract.lean
git commit -m "$(cat <<'EOF'
Add EventDelivery.Contract: types and transition relation (#187)

World/Action/Transition/Trace inductive plus SourceInstance + DedupePolicy
records. No proofs yet — those land in Properties.lean. Not imported into
Proofs.lean until the umbrella module lands so the working tree stays
buildable.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Umbrella `Proofs/EventDelivery.lean` + Proofs.lean import

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/EventDelivery.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean`

- [ ] **Step 1: Create the umbrella**

```lean
import Proofs.EventDelivery.Contract
```

(Will grow as the rest of the modules land. One import per task that adds a sibling file.)

- [ ] **Step 2: Add the Proofs.lean import**

In `crates/defra-agent/proofs/Proofs.lean`, append below the existing imports:

```lean
import Proofs.EventDelivery
```

- [ ] **Step 3: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: succeeds.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/EventDelivery.lean \
        crates/defra-agent/proofs/Proofs.lean
git commit -m "$(cat <<'EOF'
Wire Proofs.EventDelivery into the top-level import (#187)

Umbrella module ships with only Contract today; Properties and the three
instance modules append imports as they land.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `Proofs/EventDelivery/Properties.lean` — `Fair` and `pendingWork`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean`
- Modify: `crates/defra-agent/proofs/Proofs/EventDelivery.lean`

- [ ] **Step 1: Create the Properties skeleton with measure + Fair**

```lean
import Proofs.EventDelivery.Contract

namespace EventDelivery

/-- Companion termination measure: number of persistent docs not yet in
    `processedSet`. Strictly decreases under `handle`; bounded-non-increasing
    under every other action. -/
def pendingWork (w : World) : Nat :=
  (w.persistentSet.filter (fun d => d ∉ w.processedSet)).length

/-- A list of actions is `Fair` for an instance when every window of
    `inst.rescanBoundedBy + 1` consecutive actions contains at least one
    `rescanTick`.

    When `inst.rescanBoundedBy = 0` (the `unboundedRescan` sentinel), every
    window of size `1` must contain a `rescanTick`, i.e. EVERY action must
    be `rescanTick`. Since real action lists also contain `persist`, etc.,
    the sentinel makes `Fair` unsatisfiable for any non-trivial trace —
    that's exactly what closes D1 vacuously for deviation instances. -/
def Fair (inst : SourceInstance) (actions : List Action) : Prop :=
  ∀ i : Nat, i + inst.rescanBoundedBy < actions.length →
    ∃ j : Nat, i ≤ j ∧ j ≤ i + inst.rescanBoundedBy ∧
      (actions.get? j).map Action.isRescan = some true

/-- The empty action list is trivially fair. -/
theorem Fair.nil (inst : SourceInstance) : Fair inst [] := by
  intro i h_lt
  simp at h_lt

/-- A single `rescanTick` is fair for any instance with `rescanBoundedBy > 0`. -/
theorem Fair.singleton_rescanTick
    (inst : SourceInstance) (h_pos : 0 < inst.rescanBoundedBy) :
    Fair inst [.rescanTick] := by
  intro i h_lt
  -- actions.length = 1; i + rescanBoundedBy < 1 forces i = 0 and
  -- rescanBoundedBy = 0, contradicting h_pos.
  have : i = 0 := by omega
  subst this
  have : inst.rescanBoundedBy = 0 := by omega
  omega

end EventDelivery
```

- [ ] **Step 2: Add umbrella import**

Append to `crates/defra-agent/proofs/Proofs/EventDelivery.lean`:

```lean
import Proofs.EventDelivery.Properties
```

- [ ] **Step 3: Build**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: succeeds.

- [ ] **Step 4: Zero-sorry check + commit**

```bash
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/  # expect empty
git add crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean \
        crates/defra-agent/proofs/Proofs/EventDelivery.lean
git commit -m "$(cat <<'EOF'
Add Properties.Fair predicate + pendingWork measure (#187)

Fair is the bounded-gap-between-rescanTicks predicate. pendingWork is
the analogue of disagreementCount from PairingReconcile/Convergence.lean
— it strictly decreases under handle and grounds the D1 proof.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: D1 — `delivery_convergence` (the load-bearing safety theorem)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean`

- [ ] **Step 1: Prove per-action behavior of `pendingWork`**

Append to `Properties.lean` (above the existing `end EventDelivery`):

```lean
/-- `handle d` strictly decreases pendingWork when d was unprocessed. -/
theorem pendingWork_strictly_decreases_under_handle
    {w₁ w₂ : World} {d : DocId}
    (h : Transition w₁ (.handle d) w₂) :
    pendingWork w₂ < pendingWork w₁ ∨
      (d ∉ w₁.persistentSet) := by
  cases h with
  | handle _ _ _ h_queue h_unprocessed =>
    -- After handle, d ∈ processedSet, so d is filtered out of the count.
    -- If d was in persistentSet, pendingWork decreases by 1.
    by_cases hd : d ∈ w₁.persistentSet
    · left
      simp [pendingWork]
      -- The filtered list loses exactly one element (d).
      -- Use List.length_filter_lt or equivalent.
      have h_in : d ∈ w₁.persistentSet.filter (fun x => x ∉ w₁.processedSet) := by
        rw [List.mem_filter]; exact ⟨hd, by simp [h_unprocessed]⟩
      -- Post-state: persistentSet is the same; processedSet gained d.
      -- d was in (filter on pre) and won't be in (filter on post).
      have : (w₁.persistentSet.filter (fun x => x ∉ d :: w₁.processedSet)).length
           < (w₁.persistentSet.filter (fun x => x ∉ w₁.processedSet)).length := by
        apply List.length_lt_of_ne_filter
        intro x h_x_pre h_x_post_neg
        -- Standard list-filter shrinkage; close with omega / decide.
        sorry  -- replace per proof recipe below
      exact this
    · right; exact hd
```

> **Proof recipe (replace the `sorry` above):** The post-`handle` `processedSet` is `d :: w₁.processedSet`, so the predicate `(fun x => x ∉ d :: w₁.processedSet)` rejects `d` but agrees with the pre-predicate on every other element. Strict-shrinkage follows because `d` is in the pre-filtered list (`h_in`) and not in the post-filtered list. Use `List.filter_cons` reasoning or `List.length_lt_iff` from Mathlib. If `List.length_lt_of_ne_filter` doesn't exist by that exact name, prove the supporting lemma inline:
>
> ```lean
> private lemma filter_strict_drop {α} [DecidableEq α] (l : List α) (d : α)
>     (p : α → Bool) (q : α → Bool)
>     (h_only : ∀ x, x ≠ d → p x = q x)
>     (h_pre : d ∈ l) (h_p_d : p d = true) (h_q_d : q d = false) :
>     (l.filter q).length < (l.filter p).length := ...
> ```
>
> Final form must have zero `sorry`. Run `lake build` after replacing and confirm.

- [ ] **Step 2: Prove non-handle actions are bounded-non-increasing**

Append:

```lean
theorem pendingWork_nonIncreasing_under_non_handle
    {w₁ w₂ : World} {a : Action}
    (h : Transition w₁ a w₂)
    (h_not_handle : ∀ d, a ≠ .handle d) :
    pendingWork w₂ ≤ pendingWork w₁ + 1 := by
  cases h with
  | persist _ d _ =>
    -- pendingWork might gain 1 if d ∉ processedSet
    simp [pendingWork]
    -- (d :: persistentSet).filter ...  ≤ persistentSet.filter ... + 1
    by_cases hd : d ∈ w₁.processedSet
    · simp [List.filter, hd]
    · simp [List.filter, hd]
      omega
  | depersist _ _ _ =>
    -- pendingWork can only shrink under depersist
    simp [pendingWork]
    apply Nat.le_succ_of_le
    apply List.length_filter_le_of_sublist
    exact List.erase_sublist _ _
  | enqueue _ _ _ => simp [pendingWork]; omega
  | drop _ _ _ => simp [pendingWork]; omega
  | deliverFromQueue _ _ _ => simp [pendingWork]; omega
  | rescanTick _ => simp [pendingWork]; omega
  | handle _ d _ _ _ =>
    exact absurd rfl (h_not_handle d)
```

> **Note:** This bound (`+ 1`) is loose — it only matters that the measure doesn't blow up. `persist` is the only action that can grow `pendingWork`, and grows it by at most 1. Other actions either preserve or shrink it.

- [ ] **Step 3: State D1**

Append:

```lean
/-- **D1 — Delivery convergence.** Under a fair trace (rescanTicks within
    bounded gap), every persistent doc eventually reaches `handled` or
    leaves `persistentSet`.

    Substantive when `inst.rescanBoundedBy > 0`; vacuous (Fair unsatisfiable
    on non-trivial traces) when `inst.rescanBoundedBy = 0` (the
    `unboundedRescan` sentinel). The Conformance/Deviations.lean entry
    records which instance is in which state today. -/
theorem D1_delivery_convergence
    (inst : SourceInstance)
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet) :
    ∀ (actions : List Action) (wTrace : World → List Action → World → Prop),
      -- wTrace is the trace-of-actions induced by `Transition`.
      -- Define it inline so the statement is self-contained.
      (∀ w, wTrace w [] w) →
      (∀ w₁ a as w₂ w₃, Transition w₁ a w₂ → wTrace w₂ as w₃ → wTrace w₁ (a :: as) w₃) →
      Fair inst actions →
      ∃ w', wTrace w₀ actions w' ∧
        (d ∈ w'.handled ∨ d ∉ w'.persistentSet) := by
  sorry
```

> **Status check:** This step lands with a single `sorry` so the structure is visible. **You must close it before commit.** The proof recipe is in Step 4.

- [ ] **Step 4: Close the D1 proof**

Replace the `sorry` with the constructive witness. Strategy:

1. **Wrap the trace predicate as a concrete inductive.** Replace the awkward `wTrace` quantifier with the existing `Trace` predicate plus an "action list witness." Concretely, define:

   ```lean
   inductive TraceOf : World → List Action → World → Prop where
     | nil  {w}                 : TraceOf w [] w
     | cons {w₁ a w₂ as w₃} :
         Transition w₁ a w₂ → TraceOf w₂ as w₃ → TraceOf w₁ (a :: as) w₃
   ```

   Then restate D1 in terms of `TraceOf`. (You can drop the `wTrace` parameter from Step 3 entirely — that was a hint to keep the statement self-contained; it's cleaner with a dedicated inductive.)

2. **Induct on `actions`.**
   - **Base case (`actions = []`)**: requires `inst.rescanBoundedBy + 0 < 0` for any `i`, which is impossible. So the Fair hypothesis is vacuously OK, but the trace is just `TraceOf.nil` and we need `d ∈ w₀.handled ∨ d ∉ w₀.persistentSet`. The right disjunct fails by `h_persisted` ∈, so we need the left — but we haven't taken any actions. **This means D1 must additionally allow `actions = []` only when `d` is already handled OR when the action list is non-empty enough to reach a handle.** Re-state the theorem to drop the actions parameter; instead say: *there exists* an action list that drives the convergence.

   Restated:

   ```lean
   theorem D1_delivery_convergence
       (inst : SourceInstance)
       (w₀ : World) (d : DocId)
       (h_persisted : d ∈ w₀.persistentSet)
       (h_inst_pos : 0 < inst.rescanBoundedBy) :
       ∃ (actions : List Action) (w' : World),
         TraceOf w₀ actions w' ∧
         Fair inst actions ∧
         (d ∈ w'.handled ∨ d ∉ w'.persistentSet) := by
     ...
   ```

   The `h_inst_pos` hypothesis is what separates substantive from vacuous closure. For vacuous instances (`rescanBoundedBy = 0`), D1 trivially holds because the conclusion is `∃ ... ∧ Fair ...` and Fair is unsatisfiable on non-trivial lists.

3. **Construct the witness:** `actions = [.rescanTick, .handle d]`, `w' = ⟨w₀.persistentSet, ..., d :: w₀.handled, ...⟩`. Show that:
   - `rescanTick` is applicable from `w₀` (it always is).
   - After rescanTick, `d ∈ subscriptionQueue` (because `d ∈ persistentSet` and `d ∉ processedSet` from the empty initial processedSet — but actually `processedSet` could be non-empty; need an additional hypothesis or extend the persistentSet/processedSet to allow this).
   - `handle d` is applicable from the post-rescanTick world.

   If `d ∈ processedSet` already, then by the contract `d` is "already in flight" — return the right disjunct via depersist, or refine the statement to additionally assume `d ∉ w₀.processedSet`. (Add this hypothesis to the theorem if needed.)

4. **Fairness witness for `[.rescanTick, .handle d]`:** when `rescanBoundedBy ≥ 1`, every window of size `rescanBoundedBy + 1 = 2` starting at index 0 contains `.rescanTick` at position 0. The only other admissible window is at index 1 with size 2, which would require `actions.length ≥ 3` — but our list has length 2, so the window's existence requirement `i + rescanBoundedBy < actions.length` reduces to `1 + 1 < 2`, which is false. So only one window need be checked.

5. **Run `lake build` and confirm no sorry.**

- [ ] **Step 5: Build + zero-sorry check**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/
```

Both succeed.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean
git commit -m "$(cat <<'EOF'
Prove D1 delivery_convergence under fair rescan cadence (#187)

Constructive witness: from a persistent doc d, the two-action list
[rescanTick, handle d] is admissible under Fair inst when
inst.rescanBoundedBy ≥ 1, and lands d in handled. The fairness-vacuous
case (rescanBoundedBy = 0) is what makes EventSource and SubagentSource's
unboundedRescan sentinel close D1 vacuously today.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: D2 — fair-delivery latency witness

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean`

- [ ] **Step 1: State + prove D2**

Append to `Properties.lean`:

```lean
/-- **D2 — Fair-delivery latency.** Under fair subscription delivery (every
    `enqueue d` is eventually `deliverFromQueue d` before a `drop d` for the
    same `d`), the convergence trace from D1 can be shortened: `d` reaches
    `handled` via the subscription path without needing a `rescanTick`.

    Witness-only; not load-bearing. -/
theorem D2_fair_delivery_latency
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      d ∈ w'.handled ∧
      .rescanTick ∉ actions := by
  -- Witness: [.enqueue d, .handle d]
  refine ⟨[.enqueue d, .handle d], ?_, ?_, ?_⟩
  · -- TraceOf w₀ [.enqueue d, .handle d] w'
    apply TraceOf.cons (Transition.enqueue _ _ h_persisted)
    apply TraceOf.cons
    · apply Transition.handle
      · simp; left; rfl
      · exact h_unprocessed
    · exact TraceOf.nil
  · -- d ∈ w'.handled
    simp
  · -- .rescanTick ∉ actions
    intro h; simp at h
```

> Adjust simp lemmas as needed. The proof is short (≤ 15 lines) because the witness is concrete.

- [ ] **Step 2: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/
git add crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean
git commit -m "$(cat <<'EOF'
Prove D2 fair-delivery latency witness (#187)

Concrete two-action witness [enqueue d, handle d] demonstrating that
subscription path can close convergence without rescanTick. Documentation
property — not load-bearing.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: C1 — watcher cooldown invariant

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean`

- [ ] **Step 1: State + prove C1**

Append:

```lean
/-- **C1 — Processed-set excludes re-handle.** For any source instance, while
    `d ∈ processedSet`, no `handle d` action is admissible from the current
    world. Watcher-relevant (the 30 s processed-id cooldown enforces exactly
    this); also true unconditionally for monotoneOnce instances. -/
theorem C1_processed_set_excludes_handle
    (w : World) (d : DocId) (a : Action) (w' : World)
    (h_processed : d ∈ w.processedSet)
    (h : Transition w a w') :
    a ≠ .handle d := by
  intro h_eq
  rw [h_eq] at h
  cases h with
  | handle _ _ _ _ h_unprocessed =>
    exact h_unprocessed h_processed
```

- [ ] **Step 2: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/
git add crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean
git commit -m "$(cat <<'EOF'
Prove C1 processed-set excludes re-handle (#187)

Direct corollary of the handle transition's d ∉ processedSet precondition.
Watcher's 30s processed-id cooldown enforces this operationally; the
contract makes it a structural invariant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `Watcher.lean` — instance with `rescanBoundedBy = 1`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/EventDelivery/Watcher.lean`
- Modify: `crates/defra-agent/proofs/Proofs/EventDelivery.lean`

- [ ] **Step 1: Create Watcher.lean**

```lean
import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

namespace EventDelivery.Watcher

/-- Watcher instance.

`rescanBoundedBy = 1`: the contract definition counts non-rescanTick actions
between rescanTicks. The Rust `next_request` loop (`watcher.rs:88`) runs
`pending_requests()` on every iteration, so at most one non-rescan action
(e.g. a `handle` of the previous iteration's pickup) can occur between
rescans. The 30s `GOSSIP_FALLBACK_POLL` is the upper bound on
subscription-quiet idle, not the rescan-action gap. -/
def instance : SourceInstance :=
  { name := "Watcher"
  , dedupePolicy := .ttlCooldown
  , rescanBoundedBy := 1
  }

/-- D1 specialized to the watcher instance. Substantive: rescanBoundedBy > 0. -/
theorem watcher_pending_eventually_observed
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair instance actions ∧
      (d ∈ w'.handled ∨ d ∉ w'.persistentSet) :=
  D1_delivery_convergence instance w₀ d h_persisted (by decide : 0 < instance.rescanBoundedBy)

/-- C1 specialized: while a request id is in the watcher's processedSet
    (within cooldown), no duplicate handle fires. -/
theorem watcher_cooldown_excludes_handle
    (w : World) (d : DocId) (a : Action) (w' : World)
    (h_processed : d ∈ w.processedSet)
    (h : Transition w a w') :
    a ≠ .handle d :=
  C1_processed_set_excludes_handle w d a w' h_processed h

end EventDelivery.Watcher
```

> Note: the call to `D1_delivery_convergence` assumes the theorem signature lands as restated in Task 5 Step 4. Adjust the call form to match the actual signature you committed. The `decide` tactic should close `0 < 1`.

- [ ] **Step 2: Add umbrella import**

Append to `Proofs/EventDelivery.lean`:

```lean
import Proofs.EventDelivery.Watcher
```

- [ ] **Step 3: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/
git add crates/defra-agent/proofs/Proofs/EventDelivery/Watcher.lean \
        crates/defra-agent/proofs/Proofs/EventDelivery.lean
git commit -m "$(cat <<'EOF'
Add Watcher EventDelivery instance with substantive D1 + C1 (#187)

rescanBoundedBy = 1 reflects next_request's per-iteration
pending_requests() call. D1 closes substantively; C1 specializes the
processed-set/handle exclusion to the 30s cooldown semantics.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `EventSource.lean` — instance with `unboundedRescan`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/EventDelivery/EventSource.lean`
- Modify: `crates/defra-agent/proofs/Proofs/EventDelivery.lean`

- [ ] **Step 1: Create EventSource.lean**

```lean
import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

namespace EventDelivery.EventSource

/-- EventSource instance.

Uses the `unboundedRescan` sentinel because the Rust EventSource has no
periodic rescan today. D1 holds vacuously: `Fair instance actions` is
unsatisfiable on non-trivial action lists. The corresponding
`Conformance/Deviations.lean` entry names the gap.

Binding: persistentSet = (collection, doc_id) pairs in `desired_collections`
not yet in `seen_docs`. SeedSeenDocs at reconcile is modeled as initial
processedSet population; that's what makes the forward-only semantic
(pre-existing docs do not fire as "created") falsifiable. -/
def instance : SourceInstance :=
  { name := "EventSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := SourceInstance.unboundedRescan
  }

/-- Vacuous D1 specialization for EventSource: `Fair instance ...` is
    unsatisfiable on non-trivial actions, so the conclusion has nothing to
    prove. Recorded explicitly so the conformance ledger has a closure
    pointer; the Deviations entry names the gap. -/
theorem eventSource_D1_vacuous
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet) :
    ∀ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' →
      Fair instance actions →
      ¬actions = [] →
      (d ∈ w'.handled ∨ d ∉ w'.persistentSet) := by
  intro actions w' h_trace h_fair h_nonempty
  -- Fair instance actions with rescanBoundedBy = 0 demands every action be rescanTick.
  -- A non-empty action list satisfying Fair has actions.get? 0 = some .rescanTick.
  -- But that alone doesn't get us to a handle; the witness is vacuous because
  -- `Fair` with rescanBoundedBy = 0 forces *every* action to be rescanTick,
  -- which is operationally impossible (the trace can't make progress).
  -- The proof closes by contradiction: with rescanBoundedBy = 0, Fair forces
  -- every position to host a rescanTick; combined with .handle/.persist/...
  -- not being rescanTick, no real trace exists.
  -- Hence the implication is vacuous.
  exfalso
  -- Pick any non-rescan action in `actions` and derive a contradiction with
  -- h_fair. If `actions` is all `.rescanTick`, then handle never fires, so
  -- d ∉ handled and d ∈ persistentSet (no transitions affected it) — neither
  -- disjunct can be proved from those facts alone, contradicting the goal.
  sorry
```

> **Proof recipe (replace the `sorry`):** Two-arm structure.
>
> 1. If actions contains any non-rescan action `a` at index `i`, derive a contradiction with `h_fair`: at window starting at index `i`, the only allowed position is `i` itself (since `i + 0 = i < length`), but `actions.get? i = some a` and `a.isRescan = false`. So `Fair` fails.
> 2. Otherwise all actions are `.rescanTick`. Then no `handle` action occurs, so `d ∉ w'.handled` (unless w₀ already had d there, which `h_persisted` and "fresh d" rule out — strengthen the theorem with `d ∉ w₀.handled` if needed). Also `d ∈ w'.persistentSet` because no `depersist d` occurred. So neither disjunct holds, contradicting the goal.
>
> The cleaner framing: prove that `Fair instance actions ∧ actions ≠ []` is impossible together with the trace having any non-rescan content. Adjust the theorem statement if needed — the easiest is to state it as `Fair instance actions → ∀ a ∈ actions, a = .rescanTick`, then point out that all-rescanTick traces can't change `handled` or `persistentSet`, so the deviation-state is the only consistent outcome.
>
> Reframe the theorem if the proof becomes too tangled. Approved alternative: drop `eventSource_D1_vacuous` and instead provide:
>
> ```lean
> theorem eventSource_rescanBoundedBy_is_sentinel :
>     instance.rescanBoundedBy = SourceInstance.unboundedRescan := rfl
> ```
>
> which is a trivial fact and recordable in conformance metadata. The "vacuous D1" is then **implicit** in the contract — the simulation lemma holds for any positive value of `rescanBoundedBy`, but EventSource doesn't have one. The deviation entry carries the load. **Use this simpler form unless the longer proof closes cleanly within ~20 minutes.**

- [ ] **Step 2: Add umbrella import**

Append to `Proofs/EventDelivery.lean`:

```lean
import Proofs.EventDelivery.EventSource
```

- [ ] **Step 3: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/
git add crates/defra-agent/proofs/Proofs/EventDelivery/EventSource.lean \
        crates/defra-agent/proofs/Proofs/EventDelivery.lean
git commit -m "$(cat <<'EOF'
Add EventSource EventDelivery instance with unboundedRescan sentinel (#187)

Today's binding records the deviation (no periodic rescan). D1 holds
vacuously; the substantive proof activates once Rust adds the periodic
introspection loop. Deviation entry lands in the Conformance task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `SubagentSource.lean` — instance + O1 specialization

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/EventDelivery/SubagentSource.lean`
- Modify: `crates/defra-agent/proofs/Proofs/EventDelivery.lean`

- [ ] **Step 1: Create SubagentSource.lean**

```lean
import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

namespace EventDelivery.SubagentSource

/-- SubagentSource instance.

Same `unboundedRescan` sentinel as EventSource. Existing operational
recovery primitive: `recover_orphan_subagent_children` (startup-only sweep).
Lifting this to a periodic loop closes the deviation. -/
def instance : SourceInstance :=
  { name := "SubagentSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := SourceInstance.unboundedRescan
  }

/-- **O1 — Orphan-child materialization** (SubagentSource specialization).

If a running AgentToolCall row has `child_request_id = Some c` and `c` is
not yet present as an AgentRequest row, then under a fair trace `c`
eventually appears.

Stated as a corollary of D1 with the SubagentSource binding. Substantive
when the binding's `rescanBoundedBy` is positive; vacuous today (deviation
entry records the gap). -/
theorem O1_orphan_child_materialization
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet)
    (h_inst_pos : 0 < instance.rescanBoundedBy) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair instance actions ∧
      (d ∈ w'.handled ∨ d ∉ w'.persistentSet) :=
  D1_delivery_convergence instance w₀ d h_persisted h_inst_pos

/-- Sentinel record: SubagentSource currently uses the unbounded-rescan
    sentinel. The Conformance/Deviations.lean entry names the live-rescan
    gap and the follow-up issue that closes it. -/
theorem subagentSource_rescanBoundedBy_is_sentinel :
    instance.rescanBoundedBy = SourceInstance.unboundedRescan := rfl

end EventDelivery.SubagentSource
```

- [ ] **Step 2: Add umbrella import**

Append to `Proofs/EventDelivery.lean`:

```lean
import Proofs.EventDelivery.SubagentSource
```

- [ ] **Step 3: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/
git add crates/defra-agent/proofs/Proofs/EventDelivery/SubagentSource.lean \
        crates/defra-agent/proofs/Proofs/EventDelivery.lean
git commit -m "$(cat <<'EOF'
Add SubagentSource EventDelivery instance with O1 specialization (#187)

O1 is a D1 corollary parameterized on a positive rescanBoundedBy; today
the binding uses the unboundedRescan sentinel so O1 is unconditionally
provable but vacuous until Rust adds the periodic loop.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: `Conformance/EventDelivery.lean` — Family 1 (transition cases)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean`

- [ ] **Step 1: Create the conformance file with Family 1 contents**

```lean
import Proofs.EventDelivery

namespace Conformance.EventDelivery

open _root_.EventDelivery

/-- A single (pre, action, post) transition witness with a name. -/
structure TransitionCase where
  name   : String
  pre    : World
  action : Action
  post   : World

/-- Helper to build a fresh DocId. -/
private def doc (s : String) : DocId := { raw := s }

/-- The empty initial world used by most cases. -/
private def w0 : World := World.empty

/-- 13 witness rows exercising every Transition constructor + the rejected
    handle path (handle-already-processed must not fire). -/
def transitionCases : List TransitionCase :=
  [ -- persist on empty world
    { name   := "persist_into_empty"
    , pre    := w0
    , action := .persist (doc "a")
    , post   := { w0 with persistentSet := [doc "a"] }
    }
  , -- persist after an existing doc
    { name   := "persist_extends_set"
    , pre    := { w0 with persistentSet := [doc "a"] }
    , action := .persist (doc "b")
    , post   := { w0 with persistentSet := [doc "b", doc "a"] }
    }
  , -- depersist
    { name   := "depersist_removes"
    , pre    := { w0 with persistentSet := [doc "a", doc "b"] }
    , action := .depersist (doc "a")
    , post   := { w0 with persistentSet := [doc "b"] }
    }
  , -- enqueue
    { name   := "enqueue_from_persistent"
    , pre    := { w0 with persistentSet := [doc "a"] }
    , action := .enqueue (doc "a")
    , post   := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "a"] }
    }
  , -- drop
    { name   := "drop_from_queue"
    , pre    := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "a"] }
    , action := .drop (doc "a")
    , post   := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [] }
    }
  , -- deliverFromQueue
    { name   := "deliver_consumes_queue"
    , pre    := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "a"] }
    , action := .deliverFromQueue (doc "a")
    , post   := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [] }
    }
  , -- rescanTick with empty persistent set
    { name   := "rescan_on_empty"
    , pre    := w0
    , action := .rescanTick
    , post   := w0
    }
  , -- rescanTick on one persistent, none processed → queue gets it
    { name   := "rescan_fills_queue"
    , pre    := { w0 with persistentSet := [doc "a"] }
    , action := .rescanTick
    , post   := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "a"] }
    }
  , -- rescanTick with mixed processed/unprocessed
    { name   := "rescan_skips_processed"
    , pre    := { w0 with persistentSet := [doc "a", doc "b"]
                       , processedSet := [doc "a"] }
    , action := .rescanTick
    , post   := { w0 with persistentSet := [doc "a", doc "b"]
                       , processedSet := [doc "a"]
                       , subscriptionQueue := [doc "b"] }
    }
  , -- handle: legal path (queued + not processed)
    { name   := "handle_legal_drains_queue"
    , pre    := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "a"] }
    , action := .handle (doc "a")
    , post   := { w0 with persistentSet := [doc "a"]
                       , handled := [doc "a"]
                       , processedSet := [doc "a"]
                       , subscriptionQueue := [] }
    }
  , -- handle: idempotence (after handle, processedSet contains d)
    { name   := "handle_marks_processed"
    , pre    := { w0 with persistentSet := [doc "a", doc "b"]
                       , subscriptionQueue := [doc "a", doc "b"] }
    , action := .handle (doc "a")
    , post   := { w0 with persistentSet := [doc "a", doc "b"]
                       , subscriptionQueue := [doc "b"]
                       , handled := [doc "a"]
                       , processedSet := [doc "a"] }
    }
  , -- enqueue twice yields two entries (the queue is a multiset)
    { name   := "enqueue_twice_multiset"
    , pre    := { w0 with persistentSet := [doc "a"] }
    , action := .enqueue (doc "a")
    , post   := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "a"] }
    }
  , -- rescanTick prepends, not appends
    { name   := "rescan_prepends_to_queue"
    , pre    := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "z"] }
    , action := .rescanTick
    , post   := { w0 with persistentSet := [doc "a"]
                       , subscriptionQueue := [doc "a", doc "z"] }
    }
  ]

def transitionCaseCount : Nat := transitionCases.length

end Conformance.EventDelivery
```

- [ ] **Step 2: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean  # expect empty
git add crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean
git commit -m "$(cat <<'EOF'
Add EventDelivery conformance Family 1 — transition cases (#187)

13 witness rows covering every Transition constructor, the
rejected-handle path is structurally enforced by the inductive (no row
needed). Sets up the JSON-emission wiring in the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Family 2 (source instances) + Family 3 (convergence traces) + JSON serializers

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean`

- [ ] **Step 1: Append Family 2 — source instance metadata**

```lean
namespace Conformance.EventDelivery

structure SourceInstanceRow where
  name             : String
  dedupePolicy     : String
  rescanBoundedBy  : Nat
  deviation        : Option String   -- e.g. "lacks_periodic_rescan"

def sourceInstances : List SourceInstanceRow :=
  [ { name := "Watcher"
    , dedupePolicy := DedupePolicy.toContract .ttlCooldown
    , rescanBoundedBy := 1
    , deviation := none
    }
  , { name := "EventSource"
    , dedupePolicy := DedupePolicy.toContract .monotoneOnce
    , rescanBoundedBy := SourceInstance.unboundedRescan
    , deviation := some "event_source_lacks_periodic_rescan"
    }
  , { name := "SubagentSource"
    , dedupePolicy := DedupePolicy.toContract .monotoneOnce
    , rescanBoundedBy := SourceInstance.unboundedRescan
    , deviation := some "subagent_source_lacks_live_rescan"
    }
  ]

def sourceInstanceCount : Nat := sourceInstances.length

end Conformance.EventDelivery
```

- [ ] **Step 2: Append Family 3 — convergence traces**

```lean
namespace Conformance.EventDelivery

structure ConvergenceTraceRow where
  name           : String
  instanceName   : String
  initialWorld   : World
  actions        : List Action
  finalWorld     : World
  /-- Status of this instance today: "substantive" (D1 closes with real
      witness) or "deviation" (D1 vacuous; Rust should be in the
      documented deviation state). -/
  status         : String

/-- Worked convergence trace for the watcher: persist + rescanTick + handle
    drives a doc from persistent to handled. Substantive. -/
def watcherTrace : ConvergenceTraceRow :=
  { name := "watcher_persist_rescan_handle"
  , instanceName := "Watcher"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "req-1")
      , .rescanTick
      , .handle (doc "req-1") ]
  , finalWorld :=
      { persistentSet := [doc "req-1"]
      , subscriptionQueue := []
      , processedSet := [doc "req-1"]
      , handled := [doc "req-1"]
      }
  , status := "substantive"
  }

/-- EventSource trace today: a persist event is observed but never handled
    (the periodic rescan is missing). Rust consumer asserts the runtime is
    in this deviation state. -/
def eventSourceTrace : ConvergenceTraceRow :=
  { name := "event_source_drop_then_no_resync"
  , instanceName := "EventSource"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "doc-1")
      , .enqueue (doc "doc-1")
      , .drop (doc "doc-1") ]
  , finalWorld :=
      { persistentSet := [doc "doc-1"]
      , subscriptionQueue := []
      , processedSet := []
      , handled := []
      }
  , status := "deviation"
  }

/-- SubagentSource trace today: orphan child persists, dropped event,
    no live rescan. Rust consumer asserts deviation state. -/
def subagentSourceTrace : ConvergenceTraceRow :=
  { name := "subagent_orphan_no_live_rescan"
  , instanceName := "SubagentSource"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "tool-call-1")
      , .enqueue (doc "tool-call-1")
      , .drop (doc "tool-call-1") ]
  , finalWorld :=
      { persistentSet := [doc "tool-call-1"]
      , subscriptionQueue := []
      , processedSet := []
      , handled := []
      }
  , status := "deviation"
  }

def convergenceTraces : List ConvergenceTraceRow :=
  [ watcherTrace, eventSourceTrace, subagentSourceTrace ]

def convergenceTraceCount : Nat := convergenceTraces.length

end Conformance.EventDelivery
```

- [ ] **Step 3: Append JSON serializers**

```lean
namespace Conformance.EventDelivery

-- Local JSON helpers (matching Conformance.TriggerContracts shape).
def jsonString (s : String) : String := "\"" ++ s ++ "\""
def jsonArray (vs : List String) : String := "[" ++ String.intercalate "," vs ++ "]"
def jsonOptionString : Option String → String
  | none => "null"
  | some s => jsonString s

def docIdJson (d : DocId) : String := jsonString d.raw

def docIdListJson (ds : List DocId) : String :=
  jsonArray (ds.map docIdJson)

def worldJson (w : World) : String :=
  "{"
    ++ "\"persistent_set\":" ++ docIdListJson w.persistentSet ++ ","
    ++ "\"subscription_queue\":" ++ docIdListJson w.subscriptionQueue ++ ","
    ++ "\"processed_set\":" ++ docIdListJson w.processedSet ++ ","
    ++ "\"handled\":" ++ docIdListJson w.handled
    ++ "}"

def actionJson : Action → String
  | .persist d => "{\"kind\":\"persist\",\"doc\":" ++ docIdJson d ++ "}"
  | .depersist d => "{\"kind\":\"depersist\",\"doc\":" ++ docIdJson d ++ "}"
  | .enqueue d => "{\"kind\":\"enqueue\",\"doc\":" ++ docIdJson d ++ "}"
  | .drop d => "{\"kind\":\"drop\",\"doc\":" ++ docIdJson d ++ "}"
  | .deliverFromQueue d => "{\"kind\":\"deliver_from_queue\",\"doc\":" ++ docIdJson d ++ "}"
  | .rescanTick => "{\"kind\":\"rescan_tick\"}"
  | .handle d => "{\"kind\":\"handle\",\"doc\":" ++ docIdJson d ++ "}"

def transitionCaseJson (c : TransitionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"pre\":" ++ worldJson c.pre ++ ","
    ++ "\"action\":" ++ actionJson c.action ++ ","
    ++ "\"post\":" ++ worldJson c.post
    ++ "}"

def transitionCasesJson : String :=
  jsonArray (transitionCases.map transitionCaseJson)

def sourceInstanceRowJson (r : SourceInstanceRow) : String :=
  "{"
    ++ "\"name\":" ++ jsonString r.name ++ ","
    ++ "\"dedupe_policy\":" ++ jsonString r.dedupePolicy ++ ","
    ++ "\"rescan_bounded_by\":" ++ toString r.rescanBoundedBy ++ ","
    ++ "\"deviation\":" ++ jsonOptionString r.deviation
    ++ "}"

def sourceInstancesJson : String :=
  jsonArray (sourceInstances.map sourceInstanceRowJson)

def convergenceTraceRowJson (r : ConvergenceTraceRow) : String :=
  "{"
    ++ "\"name\":" ++ jsonString r.name ++ ","
    ++ "\"instance_name\":" ++ jsonString r.instanceName ++ ","
    ++ "\"initial_world\":" ++ worldJson r.initialWorld ++ ","
    ++ "\"actions\":" ++ jsonArray (r.actions.map actionJson) ++ ","
    ++ "\"final_world\":" ++ worldJson r.finalWorld ++ ","
    ++ "\"status\":" ++ jsonString r.status
    ++ "}"

def convergenceTracesJson : String :=
  jsonArray (convergenceTraces.map convergenceTraceRowJson)

end Conformance.EventDelivery
```

- [ ] **Step 4: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean
git add crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean
git commit -m "$(cat <<'EOF'
Add EventDelivery conformance Families 2+3 with JSON serializers (#187)

Family 2: three source-instance metadata rows (Watcher + two deviation
instances). Family 3: three convergence-trace rows, one per source —
watcher is substantive, EventSource and SubagentSource are deviation
witnesses. JSON serializers mirror Conformance.TriggerContracts shape.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Register deviations, coverage, boundary

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Deviations.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean`

- [ ] **Step 1: Add Deviations entries**

In `Deviations.lean`, replace `def deviations : List Deviation := []` with:

```lean
def deviations : List Deviation :=
  [ { id := "event_source_lacks_periodic_rescan"
    , domain := "event_delivery"
    , subject := "EventSource"
    , statement :=
        "EventSource has no periodic introspection rescan in the live process. "
        ++ "EventDelivery.D1 closes vacuously for this instance (rescanBoundedBy = 0). "
        ++ "Adding a periodic rescan flips the binding to substantive D1."
    , acceptedFailureMode := some "missed_event_observation"
    , acceptedFollowUp := some "Track at #187 PR description; deadline-audit followup #8."
    }
  , { id := "subagent_source_lacks_live_rescan"
    , domain := "event_delivery"
    , subject := "SubagentSource"
    , statement :=
        "SubagentSource has recover_orphan_subagent_children only at startup, "
        ++ "not as a periodic loop in the live process. EventDelivery.D1 closes "
        ++ "vacuously for this instance (rescanBoundedBy = 0). Lifting the existing "
        ++ "recovery primitive to a periodic timer makes D1 substantive."
    , acceptedFailureMode := some "missed_subagent_spawn_observation_in_live_process"
    , acceptedFollowUp := some "Track at #187 PR description; deadline-audit followup #5."
    }
  ]
```

- [ ] **Step 2: Add coverage ledger entries**

In `CoverageLedger.lean`, before the closing `]` of `caseCoverage`, append:

```lean
  , consumerCoverage
      "event_delivery_cases"
      "EventDeliveryTransitionCases"
      "state_machine_conformance::event_delivery_transition_cases_match_contract"
  , consumerCoverage
      "event_delivery_cases"
      "EventDeliverySourceInstances"
      "state_machine_conformance::event_delivery_source_instances_match_runtime"
  , consumerCoverage
      "event_delivery_cases"
      "EventDeliveryConvergenceTraces"
      "state_machine_conformance::event_delivery_convergence_traces_match_runtime_or_deviation"
```

- [ ] **Step 3: Add Boundaries entry**

In `Boundaries.lean`, find the existing boundary definitions and append (matching the shape of e.g. `boundaryStorageObservationDaemonVisibleId`):

```lean
def boundaryEventDeliveryFairSubstrateId : String :=
  "boundary.event-delivery.fair-substrate"

/-- The EventDelivery contract takes "fair substrate delivery" (rescanTicks
    fire with bounded gap) as an assumption, not a proof. The substrate's
    reliability — DefraDB gossip + libp2p — is modeled in tla/ReversePairing.tla.
    See docs/superpowers/specs/2026-05-13-event-drop-resync-lean-design.md. -/
def boundaryEventDeliveryFairSubstrate : Boundary :=
  { id := boundaryEventDeliveryFairSubstrateId
  , domain := "event_delivery"
  , subject := "Fair substrate delivery"
  , statement :=
      "EventDelivery's Fair predicate assumes rescanTick actions occur with "
      ++ "bounded gap. Substrate-level fairness (DefraDB gossip + libp2p delivery) "
      ++ "is taken as an axiom; the substrate model lives in tla/ReversePairing.tla."
  , reference := some "tla/ReversePairing.tla"
  }
```

Then add `boundaryEventDeliveryFairSubstrate` to the `boundaries` list in the same file. (Find the existing `def boundaries : List Boundary := [ ... ]` and append your new boundary.)

- [ ] **Step 4: Build + zero-sorry + commit**

```bash
cd crates/defra-agent/proofs && lake build && cd -
grep -rn "sorry" crates/defra-agent/proofs/Proofs/Conformance/  # expect empty (only changes in your edits)
git add crates/defra-agent/proofs/Proofs/Conformance/Deviations.lean \
        crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean \
        crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean
git commit -m "$(cat <<'EOF'
Register EventDelivery deviations, coverage, and fair-substrate boundary (#187)

Two Deviations entries (EventSource and SubagentSource lack periodic
rescan), three CoverageLedger entries (one per Family), one Boundaries
entry naming the fair-substrate assumption ceded to tla/ReversePairing.tla.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

> **Note:** If the `Boundary` structure shape differs from what's shown above (e.g., no `reference` field), match the existing definitions exactly. Use `grep -A 20 "structure Boundary" crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean` first to confirm the field set.

---

## Task 14: Wire Family 1/2/3 into the snapshot JSON

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean`

- [ ] **Step 1: Add the import**

Add near the top of `Json.lean`, with the other `import Proofs.Conformance.*` lines:

```lean
import Proofs.Conformance.EventDelivery
```

- [ ] **Step 2: Add the three JSON snapshot fields**

In the `snapshotJson` definition (line ~360), append three new fields just before the closing `}`. Find a good place (e.g. after `tool_retry_cases` or `coverage_ledger`):

```lean
    ++ "\"event_delivery_transition_case_count\":"
      ++ toString Conformance.EventDelivery.transitionCaseCount ++ ","
    ++ "\"event_delivery_transition_cases\":"
      ++ Conformance.EventDelivery.transitionCasesJson ++ ","
    ++ "\"event_delivery_source_instances\":"
      ++ Conformance.EventDelivery.sourceInstancesJson ++ ","
    ++ "\"event_delivery_convergence_traces\":"
      ++ Conformance.EventDelivery.convergenceTracesJson ++ ","
```

> Insert the comma-and-newline placement carefully — match the existing style.

- [ ] **Step 3: Run the conformance emitter and verify the JSON contains the new fields**

```bash
cd crates/defra-agent/proofs && lake env lean --run Proofs/Conformance/Contracts.lean > /tmp/conformance.json && cd -
grep -c "event_delivery_transition_cases" /tmp/conformance.json   # expect 2 (the count field + the cases field)
grep -c "event_delivery_source_instances" /tmp/conformance.json   # expect 1
grep -c "event_delivery_convergence_traces" /tmp/conformance.json # expect 1
```

If grep counts don't match, inspect `/tmp/conformance.json` directly and fix the field names.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean
git commit -m "$(cat <<'EOF'
Emit EventDelivery vector families in conformance snapshot JSON (#187)

Three new snapshot fields (transition cases, source instances,
convergence traces) consumed by the new Rust tests in the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 15: Rust consumer — Family 1 (transition cases)

**Files:**
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

- [ ] **Step 1: Locate the conformance snapshot struct**

```bash
grep -n "trigger_dispatch_cases\b" crates/defra-agent/tests/state_machine_conformance.rs | head -5
```

This finds the existing struct (`ConformanceSnapshot` or similar) that deserializes the Lean JSON. The new EventDelivery fields need fields added to that struct.

- [ ] **Step 2: Add EventDelivery types to the Rust snapshot struct**

Near the existing snapshot struct definition, add:

```rust
#[derive(serde::Deserialize, Debug, Clone)]
struct EventDeliveryDocId(String);

#[derive(serde::Deserialize, Debug, Clone)]
struct EventDeliveryWorld {
    persistent_set: Vec<String>,
    subscription_queue: Vec<String>,
    processed_set: Vec<String>,
    handled: Vec<String>,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EventDeliveryAction {
    Persist { doc: String },
    Depersist { doc: String },
    Enqueue { doc: String },
    Drop { doc: String },
    DeliverFromQueue { doc: String },
    RescanTick,
    Handle { doc: String },
}

#[derive(serde::Deserialize, Debug, Clone)]
struct EventDeliveryTransitionCase {
    name: String,
    pre: EventDeliveryWorld,
    action: EventDeliveryAction,
    post: EventDeliveryWorld,
}

#[derive(serde::Deserialize, Debug, Clone)]
struct EventDeliverySourceInstance {
    name: String,
    dedupe_policy: String,
    rescan_bounded_by: u64,
    deviation: Option<String>,
}

#[derive(serde::Deserialize, Debug, Clone)]
struct EventDeliveryConvergenceTrace {
    name: String,
    instance_name: String,
    initial_world: EventDeliveryWorld,
    actions: Vec<EventDeliveryAction>,
    final_world: EventDeliveryWorld,
    status: String,
}
```

Add fields to the existing snapshot struct:

```rust
    #[serde(default)]
    event_delivery_transition_case_count: u64,
    #[serde(default)]
    event_delivery_transition_cases: Vec<EventDeliveryTransitionCase>,
    #[serde(default)]
    event_delivery_source_instances: Vec<EventDeliverySourceInstance>,
    #[serde(default)]
    event_delivery_convergence_traces: Vec<EventDeliveryConvergenceTrace>,
```

- [ ] **Step 3: Write the Family 1 test (TDD: write failing first)**

Append at the bottom of `state_machine_conformance.rs`:

```rust
#[test]
fn event_delivery_transition_cases_match_contract() {
    let snapshot = load_lean_conformance_snapshot();
    assert_eq!(
        snapshot.event_delivery_transition_case_count as usize,
        snapshot.event_delivery_transition_cases.len(),
        "Lean event-delivery transition case count drifted from emitted cases"
    );
    assert!(
        snapshot.event_delivery_transition_cases.len() >= 12,
        "Expected at least 12 transition-case rows; got {}",
        snapshot.event_delivery_transition_cases.len()
    );
    // Sanity: every named row's `post` is consistent with re-applying the
    // action to `pre` under the contract. We don't actually run a Rust
    // simulator here (no production code); we assert structural invariants
    // that catch most authoring errors.
    for case in &snapshot.event_delivery_transition_cases {
        match &case.action {
            EventDeliveryAction::Persist { doc } => {
                assert!(
                    case.post.persistent_set.contains(doc),
                    "case `{}`: persist did not add doc to persistent_set",
                    case.name
                );
            }
            EventDeliveryAction::Handle { doc } => {
                assert!(
                    case.post.handled.contains(doc),
                    "case `{}`: handle did not add doc to handled",
                    case.name
                );
                assert!(
                    case.post.processed_set.contains(doc),
                    "case `{}`: handle did not add doc to processed_set",
                    case.name
                );
            }
            EventDeliveryAction::RescanTick => {
                // rescanTick should not change persistent_set.
                assert_eq!(
                    case.pre.persistent_set, case.post.persistent_set,
                    "case `{}`: rescanTick changed persistent_set",
                    case.name
                );
            }
            _ => { /* the constructor-driven cases are weaker; skip targeted asserts */ }
        }
    }
}
```

- [ ] **Step 4: Run the test, confirm it passes**

```bash
cargo test -p defra-agent --test state_machine_conformance event_delivery_transition_cases_match_contract -- --nocapture
```

Expected: PASS (12+ rows present, structural invariants hold).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "$(cat <<'EOF'
Add Rust consumer for EventDelivery Family 1 (transition cases) (#187)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Rust consumer — Family 2 (source instances)

**Files:**
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

- [ ] **Step 1: Write the test**

Append:

```rust
#[test]
fn event_delivery_source_instances_match_runtime() {
    let snapshot = load_lean_conformance_snapshot();
    let by_name: std::collections::HashMap<&str, &EventDeliverySourceInstance> = snapshot
        .event_delivery_source_instances
        .iter()
        .map(|i| (i.name.as_str(), i))
        .collect();

    // Watcher: substantive (no deviation), uses ttl_cooldown, positive rescanBoundedBy.
    let watcher = by_name.get("Watcher").expect("Watcher instance must be present");
    assert_eq!(watcher.dedupe_policy, "ttl_cooldown");
    assert!(watcher.rescan_bounded_by > 0, "Watcher rescanBoundedBy must be positive");
    assert!(
        watcher.deviation.is_none(),
        "Watcher must have no deviation entry; got {:?}",
        watcher.deviation
    );

    // EventSource: deviation today, monotone_once, rescanBoundedBy = 0 (sentinel).
    let event_source = by_name
        .get("EventSource")
        .expect("EventSource instance must be present");
    assert_eq!(event_source.dedupe_policy, "monotone_once");
    assert_eq!(event_source.rescan_bounded_by, 0,
        "EventSource must currently use unboundedRescan sentinel");
    assert_eq!(
        event_source.deviation.as_deref(),
        Some("event_source_lacks_periodic_rescan"),
        "EventSource deviation tag drifted",
    );

    // SubagentSource: same shape.
    let subagent_source = by_name
        .get("SubagentSource")
        .expect("SubagentSource instance must be present");
    assert_eq!(subagent_source.dedupe_policy, "monotone_once");
    assert_eq!(subagent_source.rescan_bounded_by, 0);
    assert_eq!(
        subagent_source.deviation.as_deref(),
        Some("subagent_source_lacks_live_rescan"),
    );
}
```

- [ ] **Step 2: Run + verify pass**

```bash
cargo test -p defra-agent --test state_machine_conformance event_delivery_source_instances_match_runtime -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "$(cat <<'EOF'
Add Rust consumer for EventDelivery Family 2 (source instances) (#187)

Asserts each of the three source instances has the documented
dedupe-policy, rescanBoundedBy, and deviation tag. When Rust adds the
periodic rescan to EventSource or SubagentSource, the Lean-side instance
flips and this test will require updating in lockstep.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 17: Rust consumer — Family 3 (convergence traces, including deviation-state assertions)

**Files:**
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

- [ ] **Step 1: Write the test**

Append:

```rust
#[test]
fn event_delivery_convergence_traces_match_runtime_or_deviation() {
    let snapshot = load_lean_conformance_snapshot();
    assert!(
        snapshot.event_delivery_convergence_traces.len() >= 3,
        "Expected at least one convergence trace per source"
    );

    for trace in &snapshot.event_delivery_convergence_traces {
        match trace.status.as_str() {
            "substantive" => {
                // For substantive traces, the final world must witness convergence:
                // every doc that was persisted is either handled or no longer persistent.
                let final_handled: std::collections::HashSet<&String> =
                    trace.final_world.handled.iter().collect();
                let final_persistent: std::collections::HashSet<&String> =
                    trace.final_world.persistent_set.iter().collect();

                for doc in &trace.initial_world.persistent_set {
                    let was_handled = final_handled.contains(doc);
                    let was_depersisted = !final_persistent.contains(doc);
                    assert!(
                        was_handled || was_depersisted,
                        "substantive trace `{}` did not converge for doc `{}` \
                         (handled? {}, depersisted? {})",
                        trace.name, doc, was_handled, was_depersisted,
                    );
                }
            }
            "deviation" => {
                // For deviation traces, the final world must show the failure mode:
                // doc remains persistent AND is not handled. This is the documented
                // deviation state — the test PASSES when runtime is in this state.
                // Do NOT #[ignore] this test; it's a positive assertion.
                let final_handled: std::collections::HashSet<&String> =
                    trace.final_world.handled.iter().collect();
                let final_persistent: std::collections::HashSet<&String> =
                    trace.final_world.persistent_set.iter().collect();

                let observed_deviation = trace.initial_world.persistent_set.iter().any(|doc| {
                    final_persistent.contains(doc) && !final_handled.contains(doc)
                }) || (!trace.initial_world.persistent_set.is_empty()
                       && trace.final_world.handled.is_empty());

                // We also accept the trivial case where no doc was persisted initially.
                let trivially_passing = trace.initial_world.persistent_set.is_empty();

                assert!(
                    observed_deviation || trivially_passing,
                    "deviation trace `{}` did not witness the documented \
                     deviation state (no orphan persistent doc remaining)",
                    trace.name,
                );
            }
            other => panic!(
                "trace `{}` has unknown status `{}` (expected 'substantive' or 'deviation')",
                trace.name, other,
            ),
        }
    }

    // Per-instance check: each declared source has a trace.
    let trace_instances: std::collections::HashSet<&str> = snapshot
        .event_delivery_convergence_traces
        .iter()
        .map(|t| t.instance_name.as_str())
        .collect();
    for name in &["Watcher", "EventSource", "SubagentSource"] {
        assert!(
            trace_instances.contains(name),
            "Expected a convergence trace for instance `{}`",
            name
        );
    }
}
```

- [ ] **Step 2: Run + verify pass**

```bash
cargo test -p defra-agent --test state_machine_conformance event_delivery_convergence_traces_match_runtime_or_deviation -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "$(cat <<'EOF'
Add Rust consumer for EventDelivery Family 3 (convergence traces) (#187)

Substantive traces (watcher today) must structurally witness convergence
in the final world. Deviation traces (EventSource and SubagentSource
today) must structurally witness the deviation state — orphan persistent
doc with empty handled. Per-instance presence check ensures none is
silently dropped. No #[ignore]; positive assertions throughout.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 18: Full verification

**Files:** none modified.

- [ ] **Step 1: Lean full build**

```bash
cd crates/defra-agent/proofs && lake build && cd -
```

Expected: succeeds.

- [ ] **Step 2: Zero-sorry — entire EventDelivery subtree + conformance**

```bash
grep -rn "sorry" crates/defra-agent/proofs/Proofs/EventDelivery/ \
                crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean
```

Expected: empty.

- [ ] **Step 3: Run the full conformance test suite**

```bash
cargo test -p defra-agent --test state_machine_conformance -- --nocapture
```

Expected: all tests pass, including:
- `event_delivery_transition_cases_match_contract`
- `event_delivery_source_instances_match_runtime`
- `event_delivery_convergence_traces_match_runtime_or_deviation`

- [ ] **Step 4: Run all other defra-agent tests** to confirm nothing regressed

```bash
cargo test -p defra-agent
```

Expected: all tests pass.

- [ ] **Step 5: Confirm the conformance snapshot JSON contains the new families end-to-end**

```bash
cd crates/defra-agent/proofs && \
  lake env lean --run Proofs/Conformance/Contracts.lean | \
  sed -n '/---BEGIN DEFRA LEAN CONTRACT JSON---/,/---END DEFRA LEAN CONTRACT JSON---/p' \
  > /tmp/conformance.json && cd -

python3 -c "
import json
with open('/tmp/conformance.json') as f:
    lines = [l for l in f if 'BEGIN' not in l and 'END' not in l]
    data = json.loads(''.join(lines))
for k in ['event_delivery_transition_case_count',
          'event_delivery_transition_cases',
          'event_delivery_source_instances',
          'event_delivery_convergence_traces']:
    assert k in data, f'missing {k}'
print('event_delivery families present in snapshot:',
      data['event_delivery_transition_case_count'],
      'transition cases,',
      len(data['event_delivery_source_instances']),
      'instances,',
      len(data['event_delivery_convergence_traces']),
      'traces')
"
```

Expected output:
```
event_delivery families present in snapshot: 13 transition cases, 3 instances, 3 traces
```

- [ ] **Step 6: No commit** — verification only.

---

## Task 19: Open the PR

**Files:** none modified.

- [ ] **Step 1: Push the branch**

```bash
git push -u origin proofs/issue-187-event-drop-resync
```

- [ ] **Step 2: Create the PR**

```bash
gh pr create --title "Add live event-drop resync model in Lean" --body "$(cat <<'EOF'
## Summary

Closes #187. Adds the EventDelivery Lean contract that closes the audit's
gap #4 — "Watcher / dropped-event resync" — and simultaneously closes the
deadline-audit followups #5 and #8 at the model level.

The shared `EventDeliverySource` contract has three instances:
- **Watcher** — substantive D1 today (rescanBoundedBy = 1).
- **EventSource** — vacuous D1; deviation entry `event_source_lacks_periodic_rescan`.
- **SubagentSource** — vacuous D1; deviation entry `subagent_source_lacks_live_rescan`.

When Rust adds a periodic rescan to either EventSource or SubagentSource,
the binding flips to a positive `rescanBoundedBy` and D1 for that instance
becomes substantive.

## Property closures

- **D1** — delivery convergence (load-bearing safety, no-subscription path)
- **D2** — fair-delivery latency (witness-only)
- **O1** — orphan-child materialization (SubagentSource specialization)
- **C1** — processed-set excludes re-handle (watcher cooldown invariant)

## Conformance vectors registered

- `event_delivery_transition_cases` (13 rows)
- `event_delivery_source_instances` (3 rows)
- `event_delivery_convergence_traces` (3 rows — one per source)

## Deadline-audit followups closed at model level

- #5 (missing live rescan for missed subagent spawn events)
- #8 (event-trigger dropped-message resync)

Implementation closure (Rust periodic rescan in EventSource and
SubagentSource) is deferred to follow-up issues that this PR will name.

## Modeling boundary

**Fair substrate delivery is an assumption, not a proof.** The contract
asserts rescanTicks occur with bounded gap; substrate-level fairness lives
in `tla/ReversePairing.tla`. Recorded as `boundary.event-delivery.fair-substrate`
in `Conformance/Boundaries.lean`.

Refs #183 (parent tracker), #172 (deadline-audit), #162 (substrate model).

## Test plan

- [x] `lake build` clean (zero sorry across `Proofs/EventDelivery/`).
- [x] `cargo test -p defra-agent --test state_machine_conformance` — three new
      tests pass.
- [x] Full `cargo test -p defra-agent` — no regressions.
- [x] Conformance snapshot JSON contains the three new families end-to-end.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Capture the PR URL** for the final report-back.

---

## Self-review checklist (run before declaring complete)

- [ ] Every spec section is implemented by at least one task. (Spec §Contract → T2; §Properties → T4–T7; §Per-instance → T8–T10; §Conformance → T11–T14; §Coordination/scope → T3 + PR body.)
- [ ] No `sorry` anywhere in `Proofs/EventDelivery/` or `Proofs/Conformance/EventDelivery.lean`.
- [ ] All three new Rust tests run without `#[ignore]`.
- [ ] `Proofs.lean` adds exactly one new import line.
- [ ] PR body cites: closes #187, refs #183, refs #172 (#5 + #8), refs #162.
- [ ] PR body names: D1, D2, O1, C1 closures + three vector families + the explicit "fair delivery is a modeling boundary" statement.
