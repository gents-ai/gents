# Cross-Deployment Subagent Completion TLA+ Spec - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the TLA+ specification defined in `docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md`: an abstract model of background subagent completion projection where the parent bridge row lives on deployment A, the child request terminalizes on deployment B, and A learns through DefraDB document gossip.

**Architecture:** One main TLA+ module, `SubagentCompletion.tla`, containing the abstract model: durable B child terminal/final-response state, document-gossip observations, A-side durable observation and bridge projection, transcript notification append, coalesced wake-up queue rows, cancellation drain, and crash actions. `MCSubagentCompletion.tla` + `.cfg` provide the default TLC model. Optional sanity configs may narrow terminal kinds or crash budgets if full liveness is too large. Existing `scripts/run-tlc.sh` and `scripts/install-tools.sh` are reused unchanged.

**Tech Stack:** Raw TLA+ (no PlusCal), TLC model checker via the existing `tla2tools.jar` wrapper.

---

## Decisions made

1. **Background-only model.** Foreground cross-deployment blocking is deferred. The model covers durable notification and coalesced wake-up behavior for background children.
2. **Two child bridges.** The default model uses two children in one parent session so coalescing can be verified without modeling unbounded fanout.
3. **Document-gossip channel.** Completion delivery is modeled as a separate document-gossip channel from the reverse-pairing admin RPC lane in #162.
4. **No child claim.** B-local execution is abstracted to `PersistChildTerminal(child, terminal)`.
5. **Both A and B crash.** A crash clears volatile inbound observations; B crash preserves durable child terminal/final-response state.
6. **All child non-completed terminals project to `Failed`.** `Cancelled` is reachable only through `CancelParent`, matching the approved design update and keeping the TLA+ conformance target deterministic.
7. **B final response and terminal are atomic in the model.** `PersistChildTerminal` commits both. The real-system obligation is called out as a derived requirement because the real documents are separate: final response must be durable before or atomically with terminal observability.
8. **`cancelRequested` is a hook, not proof-critical.** The variable is written by `CancelParent` and preserved across crashes, but completion projection does not read it. It supports the deferred cancel-propagation model.

---

## What's NOT in this plan

- **R4 implementation.** Do not read or depend on `/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-design-r4-agent-facing-tools`.
- **Foreground await mode.** Deferred follow-up.
- **Cross-deployment cancellation propagation.** `CancelParent` records cascade intent, but delivery of the interrupt to B is out of scope.
- **Child claim/admission/execution.** B-local lifecycle is abstracted to a durable terminal write.
- **Multi-node harness.** The design doc sketches harness mapping; this plan lands only the TLA+ artifact and README updates.
- **Implementation issues for derived requirements.** The PR body should name them; issue filing can happen after the verified artifact is reviewed.

---

## Conventions

- **Run command:** `cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCSubagentCompletion`.
- **Safety-first iteration:** add `TypeOK` and invariants before enabling liveness. If TLC finds a violation, decide whether the transition relation is over-permissive or the property is overstated; do not weaken properties silently.
- **Commit cadence:** commit the design/plan update first, then commit each TLA+ action or tightly related action group separately. Suggested commit subjects appear in each task.
- **Working directory:** repository root, `/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-design-subagent-completion-tla`.
- **Existing untracked files:** leave unrelated untracked files such as `TLA_PROMPT.md` alone unless the user explicitly asks to commit them.
- **TLA+ style:** raw TLA+, CamelCase module names, camelCase variables, noun-form invariants (`TypeOK`, `WakeupCoalesced`), two-space indentation, stacked `/\` and `\/`.
- **Bounded-id caveat:** real event ids and queue ids are unbounded. TLC configs must leave enough headroom or use a documented `StateBound` to avoid bounded-pool liveness artifacts.

---

## File structure

Created files:

```text
crates/defra-agent/proofs/tla/
  SubagentCompletion.tla
  MCSubagentCompletion.tla
  MCSubagentCompletion.cfg
```

Optional if needed for tractability/counterexamples:

```text
crates/defra-agent/proofs/tla/
  MCSubagentCompletionSafety.cfg
  MCSubagentCompletionFailureKinds.cfg
  MCSubagentCompletionUnsafeProjection.tla
  MCSubagentCompletionUnsafeProjection.cfg
```

Modified files:

```text
docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md
docs/superpowers/plans/2026-05-12-subagent-completion-cross-deployment-tla-spec.md
crates/defra-agent/proofs/tla/README.md
```

---

## Task 1: Commit approved design and plan docs

Capture the approved design doc fixes and this implementation plan before changing the model.

**Files:**
- `docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md`
- `docs/superpowers/plans/2026-05-12-subagent-completion-cross-deployment-tla-spec.md`

- [ ] **Step 1: Review the design diff**

```bash
git diff -- docs/superpowers/specs/2026-05-12-subagent-completion-cross-deployment-tla-design.md
```

Confirm the three approval fixes are present:

- child `Interrupted` projects to parent bridge `Failed`; `Cancelled` is parent-cancel only
- `cancelRequested` is documented as a deferred cancel-propagation hook
- final response durability before terminal observability is a derived requirement

- [ ] **Step 2: Commit**

Suggested subject:

```text
Document cross-deployment subagent completion TLA design
```

---

## Task 2: Add `SubagentCompletion` skeleton, constants, variables, and `TypeOK`

Create the main module with finite domains, state variables, `Init`, `TypeOK`, and a minimal model-checking config. No behavior beyond stuttering yet.

**Files:**
- Create `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- Create `crates/defra-agent/proofs/tla/MCSubagentCompletion.tla`
- Create `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Define constants**

Use constants for:

- `Deployment`, with config `{A, B}`
- `Child`, with config `{c1, c2}`
- `EventId`, bounded pool
- `QueueId`, bounded pool
- `MaxCrashes`
- `MaxDrops`
- sentinel constant `NoTerminal`
- `ParentSession`, `CompletionQueueKey`, `UserQueueKey`

Define sets in the module:

```tla
TerminalKind == {"Completed", "Failed", "Dead", "Interrupted", "Superseded"}
BridgeState == {"Running", "Completed", "Failed", "Cancelled"}
TerminalSource == {"None", "ChildProjection", "ParentCancel"}
QueueSource == {"user", "subagent_completion"}
QueuePolicy == {"append", "coalesce"}
QueueState == {"pending", "drained"}
```

- [ ] **Step 2: Define variables**

```tla
VARIABLES
  childDurable,
  childFinalResponseDurable,
  messages,
  pendingInboundA,
  observedDurableA,
  bridgeState,
  terminalSource,
  terminalWriteCount,
  notificationDurable,
  queueRows,
  queueIdsUsed,
  eventIdsUsed,
  dropCount,
  crashCount,
  cancelRequested
```

- [ ] **Step 3: Define record sets**

`Observation` records:

```tla
[id : EventId, child : Child, terminal : TerminalKind]
```

`QueueRow` records:

```tla
[
  id      : QueueId,
  session : {ParentSession},
  source  : QueueSource,
  policy  : QueuePolicy,
  key     : {CompletionQueueKey, UserQueueKey},
  state   : QueueState
]
```

- [ ] **Step 4: Define `Init`, `vars`, `TypeOK`, `StateBound`, `Spec`**

At this stage, `Next` can be `FALSE` and `Spec == Init /\ [][Next]_vars`.

Use `terminalWriteCount \in [Child -> 0..2]` so `BridgeTerminalUnique` can catch double-writes later.

- [ ] **Step 5: Run TLC**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCSubagentCompletion
```

Expected: one initial state, `TypeOK` passes.

- [ ] **Step 6: Commit**

Suggested subject:

```text
Add SubagentCompletion TLA skeleton
```

---

## Task 3: Add B-side durable child terminal action

Add `PersistChildTerminal(child, terminal)` and invariants for single durable terminal state.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add `PersistChildTerminal`**

Guard:

- `childDurable[child] = NoTerminal`
- `terminal \in TerminalKind`

Effect:

- `childFinalResponseDurable[child] := TRUE`
- `childDurable[child] := terminal`
- all other variables unchanged

- [ ] **Step 2: Add `DurableChildTerminalOK`**

Check that final response durability accompanies every child terminal:

```tla
\A child \in Child :
  childDurable[child] # NoTerminal => childFinalResponseDurable[child]
```

- [ ] **Step 3: Add the action to `Next` and cfg**

Enable invariant:

```text
INVARIANT DurableChildTerminalOK
```

- [ ] **Step 4: Run TLC**

Expected: `TypeOK` and `DurableChildTerminalOK` pass.

- [ ] **Step 5: Commit**

Suggested subject:

```text
Model durable child terminal writes
```

---

## Task 4: Add document-gossip observation actions

Add the B-to-A document-gossip channel: emit, deliver, drop, and persist local A observations.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add helpers**

Helpers:

- `FreshEventIds(k)`
- `AllObservations == messages \cup pendingInboundA`

- [ ] **Step 2: Add `EmitTerminalObservation(child)`**

Guard:

- `childDurable[child] # NoTerminal`
- fresh `EventId` exists

Effect:

- add `[id |-> fresh, child |-> child, terminal |-> childDurable[child]]` to `messages`
- record `eventIdsUsed`

- [ ] **Step 3: Add `DeliverObservation(obs)` and `DropObservation(obs)`**

`DeliverObservation` moves an observation from `messages` to `pendingInboundA`.

`DropObservation` removes an observation from `messages` and increments `dropCount`, guarded by `dropCount < MaxDrops`.

- [ ] **Step 4: Add `PersistObservationOnA(obs)`**

Move delivered observation from `pendingInboundA` into `observedDurableA[obs.child]`.

Guard against conflicting terminals:

```tla
observedDurableA[obs.child] \in {NoTerminal, obs.terminal}
```

- [ ] **Step 5: Add invariants**

`EventIdsTracked`:

```tla
\A obs \in messages \cup pendingInboundA : obs.id \in eventIdsUsed
```

`ObservationBackedByBDurable`:

```tla
\A obs \in messages \cup pendingInboundA :
  childDurable[obs.child] = obs.terminal
  /\ childFinalResponseDurable[obs.child]
```

`ADurableObservationBackedByB`:

```tla
\A child \in Child :
  observedDurableA[child] # NoTerminal =>
    childDurable[child] = observedDurableA[child]
    /\ childFinalResponseDurable[child]
```

- [ ] **Step 6: Run TLC**

Expected: delivery safety invariants pass under drops and duplicate observations.

- [ ] **Step 7: Commit**

Suggested subject:

```text
Model child terminal document gossip
```

---

## Task 5: Add A-side bridge projection

Add `ProjectTerminal(child)` and bridge terminal safety properties.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add projection helper**

```tla
ProjectedBridgeState(t) ==
  IF t = "Completed" THEN "Completed" ELSE "Failed"
```

This intentionally maps `Failed`, `Dead`, `Interrupted`, and `Superseded` to `Failed`. `Cancelled` is not produced by child projection.

- [ ] **Step 2: Add `ProjectTerminal(child)`**

Guard:

- `bridgeState[child] = "Running"`
- `terminalSource[child] = "None"`
- `observedDurableA[child] # NoTerminal`

Effect:

- `bridgeState[child] := ProjectedBridgeState(observedDurableA[child])`
- `terminalSource[child] := "ChildProjection"`
- `terminalWriteCount[child] := terminalWriteCount[child] + 1`

- [ ] **Step 3: Add invariants**

`BridgeTerminalUnique`:

```tla
\A child \in Child : terminalWriteCount[child] <= 1
```

`ProjectionRequiresBDurableTerminal`:

```tla
\A child \in Child :
  terminalSource[child] = "ChildProjection" =>
    childDurable[child] = observedDurableA[child]
    /\ childFinalResponseDurable[child]
```

`ProjectionRequiresADurableObservation`:

```tla
\A child \in Child :
  terminalSource[child] = "ChildProjection" =>
    observedDurableA[child] # NoTerminal
```

`ProjectionMatchesLeanBridgeMapping`:

```tla
\A child \in Child :
  terminalSource[child] = "ChildProjection" =>
    bridgeState[child] = ProjectedBridgeState(observedDurableA[child])
```

`CancelledOnlyByParentCancel`:

```tla
\A child \in Child :
  bridgeState[child] = "Cancelled" => terminalSource[child] = "ParentCancel"
```

- [ ] **Step 4: Run TLC**

Expected: bridge safety invariants pass with duplicated observations and drops.

- [ ] **Step 5: Commit**

Suggested subject:

```text
Model parent bridge terminal projection
```

---

## Task 6: Add durable notification append

Add `AppendNotification(child)` and notification idempotency/causality.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add `AppendNotification(child)`**

Guard:

- `terminalSource[child] = "ChildProjection"`
- `notificationDurable[child] = FALSE`

Effect:

- `notificationDurable[child] := TRUE`

- [ ] **Step 2: Add `NotificationCausal`**

```tla
\A child \in Child :
  notificationDurable[child] => terminalSource[child] = "ChildProjection"
```

The boolean state gives idempotency by construction: duplicate projection observations cannot append two notifications.

- [ ] **Step 3: Run TLC**

Expected: all previous invariants plus notification causality pass.

- [ ] **Step 4: Commit**

Suggested subject:

```text
Model durable subagent notifications
```

---

## Task 7: Add coalesced wake-up enqueue

Add queue rows, automated wake-up coalescing, and wake-up causality.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add helpers**

Helpers:

- `FreshQueueIds(k)`
- `IsPendingCompletionWakeup(row)`
- `PendingCompletionWakeups`
- `HasPendingCompletionWakeup`

- [ ] **Step 2: Add `EnqueueWakeup(child)`**

Guard:

- `notificationDurable[child]`
- no pending automated completion wake-up exists for `(ParentSession, CompletionQueueKey)`
- fresh `QueueId` exists

Effect:

- add a row with `source = "subagent_completion"`, `policy = "coalesce"`, `state = "pending"`, `session = ParentSession`, `key = CompletionQueueKey`
- record `queueIdsUsed`

- [ ] **Step 3: Add invariants**

`QueueIdsTracked`:

```tla
\A row \in queueRows : row.id \in queueIdsUsed
```

`WakeupCoalesced`:

```tla
\A r1, r2 \in queueRows :
  r1 # r2
  /\ IsPendingCompletionWakeup(r1)
  /\ IsPendingCompletionWakeup(r2)
  => FALSE
```

`WakeupCausal`:

```tla
\A row \in queueRows :
  IsPendingCompletionWakeup(row) =>
    \E child \in Child : notificationDurable[child]
```

- [ ] **Step 4: Run TLC**

Expected: two child notifications can share one pending wake-up; no duplicate pending wake-ups appear.

- [ ] **Step 5: Commit**

Suggested subject:

```text
Model coalesced completion wakeups
```

---

## Task 8: Add user queue rows and cancellation drain

Make cancellation drain safety non-vacuous by adding user-originated pending rows, then model drain of automated wake-ups only.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add `EnqueueUserRequest`**

Guard:

- no user row exists yet, or a small user-row bound leaves room
- fresh `QueueId` exists

Effect:

- add a row with `source = "user"`, `policy = "append"`, `state = "pending"`, `session = ParentSession`, `key = UserQueueKey`

- [ ] **Step 2: Add `CancelDrain`**

Guard:

- at least one pending automated completion wake-up exists

Effect:

- set pending automated completion wake-up rows to `state = "drained"`
- leave every user row unchanged

- [ ] **Step 3: Add `UserPendingPreserved`**

```tla
\A row \in queueRows :
  row.source = "user" => row.state = "pending"
```

- [ ] **Step 4: Run TLC**

Expected: drain can remove automated wake-up work but cannot change user pending rows.

- [ ] **Step 5: Commit**

Suggested subject:

```text
Model cancellation drain queue behavior
```

---

## Task 9: Add parent cancellation and late-terminal absorption

Add `CancelParent(child)` and invariants proving cancellation wins if it terminalizes first.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add `CancelParent(child)`**

Guard:

- `bridgeState[child] = "Running"`
- `terminalSource[child] = "None"`

Effect:

- `bridgeState[child] := "Cancelled"`
- `terminalSource[child] := "ParentCancel"`
- `terminalWriteCount[child] := terminalWriteCount[child] + 1`
- `cancelRequested[child] := TRUE`

- [ ] **Step 2: Add invariants**

`ParentCancelAbsorbsLateTerminal`:

```tla
\A child \in Child :
  terminalSource[child] = "ParentCancel" =>
    /\ bridgeState[child] = "Cancelled"
    /\ terminalWriteCount[child] = 1
    /\ notificationDurable[child] = FALSE
```

`CancelRequestedCausal`:

```tla
\A child \in Child :
  cancelRequested[child] => terminalSource[child] = "ParentCancel"
```

- [ ] **Step 3: Run TLC**

Expected: if child terminal arrives after cancellation, `ProjectTerminal` is disabled and notification/wake-up do not appear for that child.

- [ ] **Step 4: Commit**

Suggested subject:

```text
Model parent cancellation winning completion races
```

---

## Task 10: Add crash actions

Add A/B crash behavior and crash bounds.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add `CrashA`**

Guard:

- `crashCount[A] < MaxCrashes` using the deployment model value from the `.cfg`

Effect:

- `pendingInboundA := {}`
- increment A crash count
- preserve all durable A and B state

- [ ] **Step 2: Add `CrashB`**

Guard:

- `crashCount[B] < MaxCrashes` using the deployment model value from the `.cfg`

Effect:

- increment B crash count
- preserve `childDurable` and `childFinalResponseDurable`

- [ ] **Step 3: Run TLC**

Expected: all safety invariants pass under crash interleavings.

- [ ] **Step 4: Commit**

Suggested subject:

```text
Model A and B crash recovery state
```

---

## Task 11: Add fairness and liveness properties

Enable temporal checking after safety is stable.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCompletion.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCompletion.cfg`

- [ ] **Step 1: Add fairness**

Use weak fairness on durable/recovery worker actions that should eventually run when continuously enabled:

- per-child `EmitTerminalObservation(child)`
- existential `DeliverObservation`
- existential `PersistObservationOnA`
- per-child `ProjectTerminal(child)`
- per-child `AppendNotification(child)`
- per-child `EnqueueWakeup(child)`

No fairness on `PersistChildTerminal`, `DropObservation`, `CrashA`, `CrashB`, `CancelParent`, `CancelDrain`, or `EnqueueUserRequest`.

- [ ] **Step 2: Add liveness properties**

`DurableTerminalSettles`:

```tla
\A child \in Child :
  childDurable[child] # NoTerminal
  /\ terminalSource[child] = "None"
    ~> terminalSource[child] = "ChildProjection"
       \/ terminalSource[child] = "ParentCancel"
```

`LiveBridgeTerminalProjects`:

```tla
\A child \in Child :
  childDurable[child] # NoTerminal
  /\ bridgeState[child] = "Running"
  /\ terminalSource[child] = "None"
    ~> terminalSource[child] = "ChildProjection"
       \/ terminalSource[child] = "ParentCancel"
```

`ProjectionNotifies`:

```tla
\A child \in Child :
  terminalSource[child] = "ChildProjection"
    ~> notificationDurable[child]
```

`ProjectionWakeupRepresented`:

```tla
\A child \in Child :
  notificationDurable[child]
    ~> HasPendingCompletionWakeup
```

- [ ] **Step 3: Tune default config**

Start with:

- `Child = {c1, c2}`
- `TerminalKind` fixed in the module
- `EventId = {e1, e2, e3, e4, e5, e6}`
- `QueueId = {q1, q2, q3, q4}`
- `MaxCrashes = 1`
- `MaxDrops = 1`
- `StateBound` limiting consumed event/queue ids with headroom

If temporal checking explodes, first reduce `MaxCrashes` for liveness and add a safety-only crash config. Do not reduce to one child in the default liveness config unless coalescing is covered by a separate config.

- [ ] **Step 4: Run TLC**

Capture:

- generated states
- distinct states
- depth
- runtime
- any temporal-property branch count shown by TLC

- [ ] **Step 5: Commit**

Suggested subject:

```text
Verify subagent completion liveness
```

---

## Task 12: Add optional unsafe-projection counterexample

Demonstrate the derived persist-before-projection obligation if tractable without bloating the main model.

**Files:**
- Optional create `crates/defra-agent/proofs/tla/MCSubagentCompletionUnsafeProjection.tla`
- Optional create `crates/defra-agent/proofs/tla/MCSubagentCompletionUnsafeProjection.cfg`
- Or document the mutant locally without committing if it is only used during analysis

- [ ] **Step 1: Add an unsafe action variant**

The unsafe variant allows projection directly from `pendingInboundA` before `PersistObservationOnA`.

- [ ] **Step 2: Check expected failure**

Expected counterexample shape:

1. B persists child terminal.
2. Observation reaches A volatile inbound queue.
3. Unsafe projection terminalizes the parent bridge.
4. A crashes and loses volatile observation.
5. `ProjectionRequiresADurableObservation` fails.

- [ ] **Step 3: Decide whether to commit**

Commit only if the unsafe module/config is small and useful as a living counterexample. Otherwise, record the trace shape in the README and PR body.

Suggested subject if committed:

```text
Document unsafe projection counterexample
```

---

## Task 13: Update TLA README

Document the new model, commands, expected TLC output, limitations, and derived requirements.

**Files:**
- `crates/defra-agent/proofs/tla/README.md`

- [ ] **Step 1: Add `SubagentCompletion` to the Specs list**

Link both design and plan docs.

- [ ] **Step 2: Add run command**

```bash
./scripts/run-tlc.sh MCSubagentCompletion
```

- [ ] **Step 3: Document checked properties**

List active invariants and temporal properties from `MCSubagentCompletion.cfg`.

- [ ] **Step 4: Document recorded run**

Include state count, depth, runtime, Java/TLC version, and hardware class.

- [ ] **Step 5: Document limitations**

Call out any scoped-down configs, excluded unsafe counterexample module, bounded-id `StateBound`, and foreground/cancel-propagation deferrals.

- [ ] **Step 6: Run TLC one final time**

Make sure README numbers match the final command output.

- [ ] **Step 7: Commit**

Suggested subject:

```text
Document SubagentCompletion TLA runs
```

---

## Task 14: Open PR

Open a PR titled `Design: TLA+ spec for cross-deployment subagent completion projection`.

- [ ] **Step 1: Inspect final diff**

```bash
git status --short
git diff --stat
```

Make sure `TLA_PROMPT.md` remains untracked unless intentionally excluded from the PR.

- [ ] **Step 2: Open PR**

PR body shape should match #162:

- summary
- notable deviations from plan
- state-space numbers and runtime
- properties enforced
- properties documented but excluded, with reasons
- derived requirements
- JDK/TLC requirement reminder
- test plan

Required references:

- `Closes #175`
- cite #162
- cite #155
- cite #168

- [ ] **Step 3: Report back**

Report:

- PR number
- state-space numbers
- properties enforced
- properties documented but excluded
- derived requirements that should become follow-up issues
