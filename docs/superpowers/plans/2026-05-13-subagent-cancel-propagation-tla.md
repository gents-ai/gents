# Cross-Deployment Subagent Cancel Propagation TLA+ Spec - Implementation Plan

> **For agentic workers:** Use the approved design in `docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-tla-design.md`. Implement task-by-task with TLC checks after each behavioral slice. Do not modify `SubagentCompletion.tla` semantically.

**Goal:** Land a sibling TLA+ artifact for #188 that verifies cross-deployment delivery of a cascade interrupt from parent deployment A to child deployment B under bounded drops, crashes, timeouts, retry, duplicate delivery, and natural child terminal races.

**Architecture:** One main TLA+ module, `SubagentCancelPropagation.tla`, containing durable A cancel intent, ReversePairing-style request-response channel state, durable B child request state, cancel-handler idempotency, natural terminal races, crash actions, safety invariants, and liveness. `MCSubagentCancelPropagation.tla` plus `.cfg` provide the default TLC model. Optional larger configs may add a second child or larger crash/drop bounds.

**Tech Stack:** Raw TLA+ and TLC via the existing `crates/defra-agent/proofs/tla/scripts/run-tlc.sh` wrapper.

---

## Decisions made

1. **Sibling spec.** Do not extend or refactor `SubagentCompletion.tla`.
2. **Cascade only.** Detach policy is out of scope.
3. **Delivery boundary only.** The Lean bridge transition is abstracted to `InvokeBridgeCancelCascade(child)`.
4. **ReversePairing-style channel.** Use `inFlight`, `messages`, `pendingInbound`, `Drop`, `Timeout`, `Crash`, `Process`, and `ReceiveAck`.
5. **B-side durable handling marker.** `cancelHandledB[child]` is the safety boundary; A ack is retry retirement and observability.
6. **Natural terminal race in scope.** Natural child terminal before cancel delivery is stable; late cancel is absorbed.
7. **Default run prioritizes liveness tractability.** Start with `Child = {c1}`, `MaxCrashes = 1`, `MaxDrops = 1`; add a bigger sanity config only if practical.

---

## What's NOT in this plan

- Production Rust.
- Lean files.
- Semantic edits to `SubagentCompletion.tla`.
- Foreground await-mode parent progress.
- Detach policy.
- Arbitrary descendant fanout on B.
- Multi-node harness implementation.

---

## Conventions

- **Run command:** `cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCSubagentCancelPropagation`.
- **Safety-first iteration:** Add `TypeOK` and safety invariants before liveness. When TLC reports a violation, fix the model or property deliberately; do not weaken a property silently.
- **Commit cadence:** Commit design/plan docs first, then commit each action or tightly related action group separately after TLC passes.
- **Working directory:** Repository root.
- **Untracked files:** Leave unrelated untracked files such as `PROMPT.md` alone.
- **TLA+ style:** Raw TLA+, CamelCase module names, camelCase variables, noun-form invariants, two-space indentation, stacked `/\` and `\/`.
- **Bounded-id caveat:** Real RPC ids are unbounded. TLC configs should leave headroom or document support properties excluded because of finite id-pool artifacts.

---

## File structure

Created files:

```text
crates/defra-agent/proofs/tla/
  SubagentCancelPropagation.tla
  MCSubagentCancelPropagation.tla
  MCSubagentCancelPropagation.cfg
```

Optional if tractable:

```text
crates/defra-agent/proofs/tla/
  MCSubagentCancelPropagationTwoChild.tla
  MCSubagentCancelPropagationTwoChild.cfg
```

Modified files:

```text
docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-tla-design.md
docs/superpowers/plans/2026-05-13-subagent-cancel-propagation-tla.md
crates/defra-agent/proofs/tla/README.md
```

---

## Task 1: Commit approved design and plan docs

Capture the approved design and this implementation plan before adding TLA+ source.

**Files:**
- `docs/superpowers/specs/2026-05-13-subagent-cancel-propagation-tla-design.md`
- `docs/superpowers/plans/2026-05-13-subagent-cancel-propagation-tla.md`

- [ ] **Step 1: Review the docs**

Confirm the design names:

- sibling spec, not `SubagentCompletion.tla` extension
- request-response cancel lane
- B-side durable handling marker
- natural terminal race
- detach policy out of scope

- [ ] **Step 2: Commit**

Suggested subject:

```text
Document subagent cancel propagation TLA design
```

---

## Task 2: Add skeleton, constants, variables, `Init`, and `TypeOK`

Create the main module and model-checking harness with no behavioral actions beyond stutter.

**Files:**
- Create `crates/defra-agent/proofs/tla/SubagentCancelPropagation.tla`
- Create `crates/defra-agent/proofs/tla/MCSubagentCancelPropagation.tla`
- Create `crates/defra-agent/proofs/tla/MCSubagentCancelPropagation.cfg`

- [ ] **Step 1: Define constants**

Use:

- `Deployment`, with default `{A, B}`
- `ParentDeployment`, default `A`
- `ChildDeployment`, default `B`
- `Child`, default `{c1}`
- `RPCId`, bounded pool
- `MaxCrashes`
- `MaxDrops`
- `NoOf`

- [ ] **Step 2: Define sets**

```tla
ChildState == {"Running", "Completed", "Failed", "Dead", "Superseded", "Interrupted"}
NaturalTerminal == {"Completed", "Failed", "Dead", "Superseded"}
TerminalSource == {"None", "Natural", "CascadeCancel"}
RPCKind == {"Cancel", "Ack"}
```

- [ ] **Step 3: Define variables**

```tla
cancelIntentA,
cancelAckedA,
cancelAttemptCountA,
childStateB,
terminalSourceB,
terminalWriteCountB,
cancelHandledB,
cancelHandleCountB,
inFlight,
messages,
pendingInbound,
rpcIdsUsed,
dropCount,
crashCount
```

- [ ] **Step 4: Define `RPC`, `vars`, `Init`, `TypeOK`, `Next`, `Spec`**

At this stage, `Next == FALSE` and `Spec == Init /\ [][Next]_vars`.

- [ ] **Step 5: Run TLC**

Expected: one initial state; `TypeOK` passes.

- [ ] **Step 6: Commit**

Suggested subject:

```text
Add SubagentCancelPropagation TLA skeleton
```

---

## Task 3: Add A-side cascade intent and retry emission

Add durable intent plus cancel RPC attempt emission from A.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCancelPropagation.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCancelPropagation.cfg`

- [ ] **Step 1: Add helpers**

- `FreshIds(k)`
- `PendingCancelFor(child)`
- `AllRPCs`

- [ ] **Step 2: Add `InvokeBridgeCancelCascade(child)`**

Guard:

- `childStateB[child] = "Running"`
- `cancelIntentA[child] = FALSE`

Effect:

- `cancelIntentA[child] := TRUE`

- [ ] **Step 3: Add `EmitCancel(child)`**

Guard:

- `cancelIntentA[child]`
- `~PendingCancelFor(child)`
- `FreshIds(1)`

Effect:

- create Cancel RPC `ParentDeployment -> ChildDeployment`
- add to `inFlight[ParentDeployment]`
- add to `messages`
- record id
- increment `cancelAttemptCountA[child]`

- [ ] **Step 4: Add invariants**

- `RPCIdsTracked`
- `RPCWellFormed`
- `CancelIntentCausal`

- [ ] **Step 5: Run TLC and commit**

Suggested subject:

```text
Model durable cascade intent emission
```

---

## Task 4: Add channel delivery, drop, timeout, and crash

Add volatile transport interleavings matching #162's substrate shape.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCancelPropagation.tla`

- [ ] **Step 1: Add `Deliver(rpc)`**

Move RPC from `messages` to `pendingInbound[rpc.tgt]`.

- [ ] **Step 2: Add `Drop(rpc)`**

Remove RPC from `messages` while `dropCount < MaxDrops`; increment `dropCount`.

- [ ] **Step 3: Add `Timeout(rpc)`**

Remove a Cancel RPC from `inFlight[ParentDeployment]`; no durable state changes.

- [ ] **Step 4: Add `Crash(deployment)`**

Clear `inFlight[deployment]` and `pendingInbound[deployment]`; preserve durable variables and `messages`; increment bounded crash counter.

- [ ] **Step 5: Run TLC and commit**

Suggested subject:

```text
Add cancel propagation channel interleavings
```

---

## Task 5: Add B-side cancel processing and ack receipt

Add the durable receiver effect and A-side ack retirement.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCancelPropagation.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCancelPropagation.cfg`

- [ ] **Step 1: Add `ProcessCancel(rpc)`**

For a Cancel RPC in `pendingInbound[ChildDeployment]`:

- remove from pending inbound
- if `cancelHandledB[child]` is false and child is `Running`, write `Interrupted`, source `CascadeCancel`, increment terminal write count, set handled, increment handle count
- if `cancelHandledB[child]` is false and child is naturally terminal, set handled and increment handle count without terminal rewrite
- if `cancelHandledB[child]` is true, no-op durable state
- emit Ack RPC with fresh id after durable handling

- [ ] **Step 2: Add `ReceiveAck(ack)`**

For Ack in `pendingInbound[ParentDeployment]`:

- require a matching in-flight Cancel by `ack.of`
- remove ack from pending inbound
- remove matched Cancel from in-flight
- set `cancelAckedA[child] := TRUE`

- [ ] **Step 3: Add invariants**

- `AckRequiresHandled`
- `CancelHandledIdempotent`
- `CascadeInterruptsOnlyRunning`
- `InterruptedOnlyByCascade`

- [ ] **Step 4: Run TLC and commit**

Suggested subject:

```text
Model B-side cascade cancel handling
```

---

## Task 6: Add natural terminal race and stability properties

Model child natural completion before remote cancel arrives.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCancelPropagation.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCancelPropagation.cfg`

- [ ] **Step 1: Add `NaturalTerminalize(child, terminal)`**

Guard:

- `childStateB[child] = "Running"`
- `terminal \in NaturalTerminal`

Effect:

- set child state to terminal
- `terminalSourceB[child] := "Natural"`
- increment `terminalWriteCountB[child]`

- [ ] **Step 2: Add invariants**

- `InterruptExactlyOnce`
- `NaturalTerminalStableAfterCancel`
- `HandledCancelStable`

- [ ] **Step 3: Run TLC and commit**

Suggested subject:

```text
Model cancel race with natural child terminal
```

---

## Task 7: Add fairness and liveness

Enable progress properties after safety passes.

**Files:**
- `crates/defra-agent/proofs/tla/SubagentCancelPropagation.tla`
- `crates/defra-agent/proofs/tla/MCSubagentCancelPropagation.cfg`

- [ ] **Step 1: Add fairness**

Use weak fairness on:

- per-child `EmitCancel(child)`
- `Deliver`
- `ProcessCancel`
- `ReceiveAck`
- `Timeout`

Do not add fairness on `Drop`, `Crash`, `InvokeBridgeCancelCascade`, or `NaturalTerminalize`.

- [ ] **Step 2: Add liveness**

- `CancelDeliveryProgress`
- `CancelAckProgress`
- `LiveCancelInterruptsOrNaturalWins`
- `CancelPropagationProgress`

- [ ] **Step 3: Add `StateBound` if needed**

Use a bounded id-pool constraint only to exclude duplicate-id churn while leaving enough headroom for convergence.

- [ ] **Step 4: Run full TLC and commit**

Suggested subject:

```text
Verify cancel propagation liveness
```

---

## Task 8: Optional two-child sanity config

Add a larger config only if the default full liveness run is comfortably under the target.

**Files:**
- Optional `crates/defra-agent/proofs/tla/MCSubagentCancelPropagationTwoChild.tla`
- Optional `crates/defra-agent/proofs/tla/MCSubagentCancelPropagationTwoChild.cfg`
- `crates/defra-agent/proofs/tla/README.md`

- [ ] **Step 1: Create config with `Child = {c1, c2}`**

Back off crashes/drops if needed for tractability.

- [ ] **Step 2: Run TLC**

Record state space and runtime.

- [ ] **Step 3: Commit if useful**

Suggested subject:

```text
Add two-child cancel propagation sanity bound
```

---

## Task 9: Update README and run final verification

Document the artifact, run commands, properties, bounded parameters, limitations, derived requirements, and recorded TLC results.

**Files:**
- `crates/defra-agent/proofs/tla/README.md`

- [ ] **Step 1: Add spec entry and run command**

Mention `SubagentCancelPropagation`, design doc, and plan doc.

- [ ] **Step 2: Add bounded parameters table**

Document default values and any bigger config.

- [ ] **Step 3: Add checked properties and fairness notes**

Separate enforced invariants from documented/excluded bounded-artifact properties.

- [ ] **Step 4: Run final verification**

At minimum:

```bash
cd crates/defra-agent/proofs/tla
./scripts/run-tlc.sh Sanity
./scripts/run-tlc.sh MCSubagentCancelPropagation
```

- [ ] **Step 5: Commit**

Suggested subject:

```text
Document cancel propagation TLA results
```

---

## Task 10: Open PR

Push the branch and open a PR titled:

```text
Design: TLA+ spec for cross-deployment subagent cancel propagation
```

PR body must include:

- `Closes #188`
- `Refs #183`
- `Refs #176`
- `Refs #155`
- notable deviations
- TLC results with state space and runtime
- properties enforced
- properties documented but excluded, if any
- derived requirements for R5 or future work

After opening, report the PR URL.
