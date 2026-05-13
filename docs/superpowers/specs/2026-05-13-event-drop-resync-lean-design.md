# Live event-drop resync model in Lean — design

Date: 2026-05-13
Issue: #187 (parent: #183, refs: #172 deadline-audit followups #5 + #8, #162 substrate)

## Why this exists

The 2026-05-13 formal coverage audit (`docs/superpowers/audits/2026-05-13-formal-coverage-audit.md`)
ranked "Watcher / dropped-event resync" as gap #4. The same gap covers two
deadline-audit followups simultaneously: #5 ("missing live rescan for missed
subagent spawn events") and #8 ("event-trigger dropped-message resync").

Today the pattern is identical at three call sites:

| | Watcher | EventSource | SubagentSource |
|---|---|---|---|
| Subscription? | yes (`EventName::Update`) | yes | yes |
| Fallback poll? | yes (30 s) | no | no |
| Dropped messages? | warn | warn | warn |
| Live rescan? | yes (built-in to `next_request` loop) | no | no |
| Cooldown / dedupe? | `processed_request_ids` w/ 30 s TTL | `seen_docs` (permanent) | `processed_tool_calls` (permanent) |
| Startup recovery? | yes (`lifecycle::recovery`) | n/a | yes (`recover_orphan_subagent_children`) |

The watcher converges; EventSource and SubagentSource converge in the live process
only by accident (subscription delivery is reliable enough most of the time) and
by hard restart (orphan recovery for SubagentSource; nothing for EventSource).

The "drop ⇒ warn ⇒ wait for fallback" pattern is a single point of formal silence
in three different files. Modeling it as a single contract closes the gap once.

## What this is not

- **Not a substrate model.** DefraDB gossip, libp2p delivery, and the `events::Bus`
  drop semantics are out of scope. That's `tla/ReversePairing.tla`'s territory.
  Substrate fairness is an explicit modeling boundary recorded in
  `Conformance/Boundaries.lean`.
- **Not a Rust implementation.** The Rust implementations of EventSource and
  SubagentSource periodic rescan are deferred to follow-up issues. This PR ships
  the model + conformance vectors; the Rust gap-fill is named in the PR body.
- **Not a refactor of `Proofs/Triggers/*`.** T1–T4 dispatch semantics are stable
  and we read them only to reuse `TriggerKind` from `Triggers/Types.lean`.
- **Not a new transcript or response state machine.** Those are #191 and the
  follow-up to that. Here we model only the delivery path.

## The shared `EventDeliverySource` contract

Every source instance is described by:

```
EventDeliverySource = {
  -- The source of truth (DefraDB)
  persistentSet      : World → Set DocId       -- docs that need attention now
  -- The (lossy) live delivery channel
  subscriptionStream : World → Stream Event    -- can drop
  -- The bounded-cadence resync path
  rescan             : World → List DocId      -- queries persistentSet
  rescanBoundedBy    : Nat                     -- max # actions between rescans
  -- The dedupe layer that lets both paths feed the same handler
  processedSet       : Set DocId               -- with TTL or monotone-once policy
}
```

The contract is parametric in two pieces:

1. **`DocId`** — opaque identifier. Each instance binds it (request_id, (collection, doc_id), tool_call_id).
2. **`DedupePolicy`** — `ttlCooldown` (watcher) or `monotoneOnce` (EventSource, SubagentSource).
   The handler precondition `d ∉ processedSet` is shared; eviction policy is per-instance.

`rescanBoundedBy : Nat` is an instance-supplied bound. Positive values feed D1 directly.
We additionally provide `SourceInstance.unboundedRescan : Nat` (concretely `0`) as a
**recording sentinel** for instances whose Rust impl does not satisfy the contract today.
D1 still holds vacuously for these instances (the `Fair` predicate is unsatisfiable when
`rescanBoundedBy = 0`), and the corresponding `Conformance/Deviations.lean` entry names
the gap. When Rust adds a periodic rescan, the instance value flips to a positive `Nat`
and the deviation flips to a positive conformance row.

## Properties to prove

### D1 — Delivery convergence (load-bearing safety)

> Under any trace whose `rescanTick` actions occur with bounded gap, every doc
> that enters `persistentSet` eventually reaches `handled` or leaves
> `persistentSet`.

```lean
theorem D1_delivery_convergence
    (inst : SourceInstance)
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet) :
    ∀ actions, Fair inst actions →
    ∃ w', Trace w₀ w' actions ∧
      (d ∈ w'.handled ∨ d ∉ w'.persistentSet)
```

Crucially, **the proof of D1 makes no use of subscription**. The proof relies
only on: (a) the `rescanTick` action eventually fires (`Fair`), (b) `rescanTick`
fills the subscription queue from `persistentSet`, (c) `handle` is then admissible
on any queued, not-yet-processed doc.

This is the same modeling stance `tla/ReversePairing.tla` takes for libp2p
delivery — fairness is asserted at the substrate level; correctness lives one
layer up. Subscription is a latency optimization, not a correctness path.

**Proof technique:** companion measure `pendingWork w := (w.persistentSet.filter (· ∉ w.processedSet)).length`,
plus per-action lemmas showing the measure is bounded-non-increasing and
strictly-decreasing under `handle`. Mirrors `phase_change_decreases_measure` and
`claimed_eventually_terminal` in `Proofs/Properties/Liveness.lean`, plus the
`disagreementCount` measure in `Proofs/PairingReconcile/Convergence.lean`.

### D2 — Fair-delivery latency (bonus, witness-only)

> Under fair subscription delivery (every `enqueue d` for a persistent doc is
> eventually `deliverFromQueue`'d before any `drop d` for the same doc), `d`
> reaches `handled` via the subscription path rather than the rescan path.

Stated as a separate theorem so it can be dropped if the proof becomes painful.
Provides a witness trace; not load-bearing.

### O1 — Orphan-child materialization (SubagentSource specialization)

> If `AgentToolCall.child_request_id = Some c` and `c` is not yet present as an
> `AgentRequest` row, then under a fair trace `c` eventually appears.

Stated by binding the contract to `SubagentSource.instance` and instantiating D1.
Closes deadline-audit followup #5.

### C1 — Processed-id cooldown invariant (watcher specialization)

> For `dedupePolicy = ttlCooldown`: while a `DocId` is in `processedSet` and
> within cooldown, no `handle` action fires for it.

Direct corollary of the `handle` constructor's `d ∉ processedSet` precondition.
Closes the audit's "processed-id cooldown invariant" obligation for the watcher.

## Module layout

```
crates/defra-agent/proofs/Proofs/EventDelivery/
├── Contract.lean       -- World, Action, Transition, Trace, SourceInstance, DedupePolicy
├── Properties.lean     -- D1, D2, O1, C1, Fair predicate, pendingWork measure
├── Watcher.lean        -- instance: pending AgentRequest pickup; ttlCooldown; closes today
├── EventSource.lean    -- instance: EventTrigger fan-out; monotoneOnce; deviation entry
├── SubagentSource.lean -- instance: orphan child materialization; monotoneOnce; deviation entry
└── Conformance.lean    -- transition cases, source instance metadata, convergence traces
```

Plus:
- `Proofs/EventDelivery.lean` — umbrella, imports the five files above (mirrors `Proofs/Triggers.lean`).
- One new line in `Proofs.lean`: `import Proofs.EventDelivery`.

This is the only edit to `Proofs.lean`. **Coordination note for #189**: that
stream may also touch `Proofs.lean` if it adds a `Proofs/Recovery/` import.
Last-to-land rebases trivially (single-line add).

## Per-instance details

### Watcher (`Watcher.lean`)

```lean
def instance : SourceInstance :=
  { name := "Watcher"
    dedupePolicy := .ttlCooldown
    rescanBoundedBy := 1  -- pending_requests() runs every loop iteration
  }
```

Why `rescanBoundedBy = 1`: `next_request` (watcher.rs:88) calls `pending_requests()`
on *every* iteration. The 30 s `GOSSIP_FALLBACK_POLL` is the upper bound on
subscription-quiet idle, not the rescan cadence itself. The model captures
worst-case cadence; for the watcher, that's one rescan per outer loop pass.

**Status: closes D1 today.** Watcher is a positive conformance entry.

### EventSource (`EventSource.lean`)

```lean
def instance : SourceInstance :=
  { name := "EventSource"
    dedupePolicy := .monotoneOnce
    rescanBoundedBy := SourceInstance.unboundedRescan
      -- sentinel for "no bounded rescan exists today"
  }
```

`rescanBoundedBy` is an abstract `Nat` in the contract; the Lean model proves
D1 for any finite value. The operational value is recorded in conformance
metadata. EventSource currently has no periodic rescan, so its instance carries
the `unboundedRescan` sentinel — D1 is still stated, but the simulation
proof for this instance is conditional on the operational rescan existing.

Binding: `persistentSet` = `(collection, doc_id)` pairs in `desired_collections`
not yet in `seen_docs`. Rescan = the periodic introspection query that this PR
asks Rust to grow. Seed at reconcile is modeled as initial `processedSet`
population — this is what makes the forward-only semantic ("pre-existing docs
do not fire as 'created'") falsifiable: if a doc was in `seen_docs` at
`subscription_open_time`, it's in `processedSet` from `world_init`, so the
`handle` precondition `d ∉ processedSet` rejects it without needing to look at
delivery history.

**Status: violates D1 today.** Lands as a `Conformance/Deviations.lean` entry:
`event_source_lacks_periodic_rescan`. The deviation cites the follow-up issue
that adds the periodic rescan; when Rust grows that, the deviation flips to a
positive conformance row.

### SubagentSource (`SubagentSource.lean`)

```lean
def instance : SourceInstance :=
  { name := "SubagentSource"
    dedupePolicy := .monotoneOnce
    rescanBoundedBy := SourceInstance.unboundedRescan
      -- live-process value; startup-only sweep is a separate boundary
  }
```

Binding: `persistentSet` = running `AgentToolCall` rows with `child_request_id` set
whose child `AgentRequest` row doesn't yet exist (the orphan condition). Rescan =
`recover_orphan_subagent_children`, which exists today as a startup-only sweep.
The instance carries `unboundedRescan` for the **live process**; the existence of
the startup sweep guarantees convergence at process boundaries but not within
a single live process. The deviation entry names the gap as "live rescan
missing"; lifting the existing recovery primitive to a periodic loop closes it.

**Status: violates D1 in the live process today; satisfies it at startup.**
Lands as a `Conformance/Deviations.lean` entry: `subagent_source_lacks_live_rescan`.
Closes O1 conditionally on the rescan being lifted to periodic.

## Conformance vectors

All emitted via `Conformance/Contracts/Json.lean`'s existing snapshot mechanism
(same pipeline as `trigger_dispatch_cases`).

### Family 1 — `event_delivery_transition_cases`

Triples `(pre : World, action : Action, post : World)` covering every
constructor of `EventDelivery.Transition`. ~12–18 finite witness rows exercising:

- `persist` from empty / non-empty world
- `rescanTick` with: empty persistent / one persistent unhandled / multiple persistent partial-handled
- `enqueue` / `drop` / `deliverFromQueue` paths
- `handle` legal (queued + not processed) and illegal (queued + already processed → rejected)
- `depersist`

Rust consumer: `event_delivery_transition_cases_match_contract` in
`tests/state_machine_conformance.rs`.

### Family 2 — `event_delivery_source_instances`

Three rows — Watcher, EventSource, SubagentSource — each carrying:

- `name : String`
- `dedupePolicy : "ttl_cooldown" | "monotone_once"`
- `rescanBoundedBy : Nat`
- `deviation : Option<String>` (`null` for watcher; deviation tag for the others)

Rust consumer: `event_delivery_source_instances_match_runtime` — asserts that the
runtime cooldown vocabulary matches and that the deviations match what the runtime
fails to satisfy today.

### Family 3 — `event_delivery_convergence_traces`

A small set of constructed convergent traces — finite witnesses to D1 — one per
source. Each row is `(initial_world, action_sequence, final_world)`. Rust runs
the trace as a sequence assertion.

This is the verifiable analogue of the watcher's existing integration test
"drop simulated, fallback poll fires, request picked up."

### Registry entries

- One entry per family in `Proofs/Conformance/CoverageLedger.lean`.
- One `boundary.event-delivery.fair-substrate` entry in `Conformance/Boundaries.lean`
  cross-referencing `tla/ReversePairing.tla`.
- Two `Conformance/Deviations.lean` entries naming the EventSource and
  SubagentSource gaps and their follow-up issues.

## Coordination with sibling streams

| Stream | Their surface | My surface | Collision |
|---|---|---|---|
| #191 (transcript) | `Proofs/Session/Transcript.lean` or `Proofs/Transcript/` | `Proofs/EventDelivery/*` | None |
| #188 (cross-deployment cancel) | `proofs/tla/*` | Lean only | None |
| #189 (Liveness extension) | `Proofs/Properties/Liveness.lean`, possibly `Proofs/Recovery/` | `Proofs/EventDelivery/*` | None on files; both edit `Proofs.lean` import list — last-to-land rebases trivially (single-line add) |
| #185 (Identity) | `Proofs/Identity/` (new) | `Proofs/EventDelivery/` (new) | None |
| #186 (MCPHealth) | `Proofs/MCPHealth/` (new) | `Proofs/EventDelivery/` (new) | None |

The only shared surface is `Proofs.lean`. The new import line is named here so
other agents see it before they rebase.

## Hard constraints honored

- **Zero `sorry`** — proof structure follows `Properties/Liveness.lean` and
  `PairingReconcile/Convergence.lean`, both of which already close their
  analogous claims constructively.
- **No Rust production code** — only conformance consumer extensions in
  `tests/state_machine_conformance.rs`.
- **Fair gossip substrate is an explicit modeling boundary** — recorded in
  `Conformance/Boundaries.lean` with the cross-reference to `tla/ReversePairing.tla`.
- **No edits to `Proofs/Triggers/*`** — read-only reuse of `TriggerKind` from
  `Triggers/Types.lean`.

## Explicit out-of-scope

- The DefraDB gossip substrate (`tla/ReversePairing.tla`).
- Per-event handler logic (`Proofs/Triggers/Dispatch.lean` — T1–T4).
- The actual Rust implementation of periodic rescan in EventSource and
  SubagentSource (separate follow-up issues).
- The watcher's `MAX_PROCESSED_IDS = 10_000` cap (operational, not formal —
  the model represents `processedSet` as an unbounded list with TTL eviction).
- Rate-limiting / priority among rescan and subscription events (the
  contract is a free schedule; the bound is on rescan cadence only).

## Property closure summary (for PR body)

When this PR lands:

- **D1** (delivery convergence) — closed, with rescan-cadence as the named assumption.
- **D2** (fair-delivery latency) — closed, optional witness trace.
- **O1** (orphan-child materialization for SubagentSource) — closed as a specialization of D1.
- **C1** (watcher processed-id cooldown invariant) — closed.
- **Deadline audit followups #5 and #8** — closed at the model level. Implementation closure deferred to follow-up issues named in the PR body.

## Conformance vectors registered

- `event_delivery_transition_cases` (~12–18 rows)
- `event_delivery_source_instances` (3 rows)
- `event_delivery_convergence_traces` (3 rows, one per source)

## Modeling boundary

**Fair substrate delivery is an assumption, not a proof.** The `Fair` predicate
on action sequences asserts that `rescanTick` fires within bounded gaps. We do
not prove that the substrate (DefraDB gossip + libp2p) achieves this — that lives
in `tla/ReversePairing.tla`. Our proof says: *if* the rescan loop runs at bounded
cadence, *then* every persisted doc is eventually observed.

The Rust obligation flowing from this: every source must have a periodic rescan
loop with a known upper-bound interval. Watcher satisfies this today;
EventSource and SubagentSource do not (deviation entries flag the gap).
