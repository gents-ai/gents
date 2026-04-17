# Frontend Tooling Audit Plan

## Goal

Turn the desktop operator/config surface into a set of reliable, auditable user journeys with real click-through coverage, then close the product gaps that those journeys expose.

The primary focus is operator/config management, not styling polish:

1. A user can create and edit an inference backend from the UI.
2. A user can create and edit the other operator config documents from the UI when the workflow requires it.
3. The key config journeys are covered by click-through tests.
4. Live journeys prove that config changes are actually used by inference, not just written to the local store.
5. Client-side operator transitions follow the same shell-state methodology as chat: explicit actions, logic-owned transitions, pure projection, and snapshot/transport separation.

## Audit Principles

- Use real user journeys as the contract.
- Prefer click-through tests over internal-state assertions.
- Use live inference where it proves that a config change affects actual behavior.
- Keep local GUI tests only when they materially speed up iteration on the same workflow.
- If the audit reveals missing CRUD behavior, add the behavior instead of hiding the gap behind narrower tests.

## Current Findings

1. Operator state handling is now much closer to the chat shell model.
   - Selection/apply/discard/run-now/new-document are explicit shell actions.
   - Side effects live in `operator/controller.rs`.
   - Snapshot rehydration lives in `operator/projection.rs`.
   - The remaining proof gap is formalization/conformance, not ad hoc UI mutation.

2. Dedicated live CRUD coverage is still uneven.
   - `Backends`: green live CRUD + behavior rebind + real inference.
   - `ScheduledTasks`: green live edit/apply/run-now/failure inspection.
   - `Behaviors`: green live edit/rebind + real inference on the active behavior.
   - `InferenceProfiles`: green live create + behavior rebind + real inference.
   - `ToolSelections`: dedicated live journey exists, but it is still noisy under live P2P replay/rate-limit pressure and does not yet have a consistently clean verdict.

3. The current multi-agent switching test is overloaded.
   - It tries to prove deployment isolation, config switching, replication, and live inference in one journey.
   - It is still red under live P2P/authorization noise.
   - The helper layer underneath it is valuable, but the test itself is too broad to be the canonical coverage unit.

4. We do not yet have an operator-shell formal/conformance layer analogous to chat.
   - `Proofs.ClientShell` already captures the right style of shell-state rules.
   - Operator should either reuse a generalized shell-state abstraction or get its own `OperatorShell` module plus Rust conformance tests.

## Canonical User Stories

### Backend Journey

1. As a user, I can open the Operator screen for a deployment.
2. As a user, I can create a new inference backend document from the UI.
3. As a user, I can edit every backend field that matters for runtime behavior:
   - backend id
   - name
   - provider kind
   - endpoint
   - API key
   - API key env var
   - max concurrent
   - max queue depth
   - enabled
   - models
   - probe status
4. As a user, I can bind a behavior to that backend from the UI.
5. As a user, a live prompt proves that the behavior is actually using the edited backend config.

Current status:
- Green locally.
- Green live on `workstation-1`.

### Profile Journey

1. As a user, I can create or edit an inference profile from the UI.
2. As a user, I can edit the meaningful runtime fields:
   - profile id
   - display name
   - context window
   - max output tokens
   - max turns
   - temperature
   - stream batch ms
   - deadline duration secs
3. As a user, I can bind a behavior to the profile and observe the change persist and replicate.

Current status:
- Green live on `workstation-1`.

### Tool Selection Journey

1. As a user, I can create or edit a tool selection from the UI.
2. As a user, I can edit file tools, bash, CLI tool names, meta tools, and delegation.
3. As a user, a live prompt can prove that the changed tool policy is actually in effect.

Current status:
- Dedicated live journey exists.
- Still noisy under live P2P replay/rate-limit pressure and does not yet have a consistently clean verdict.

### Behavior Journey

1. As a user, I can create or edit a behavior from the UI.
2. As a user, I can bind backend, model, tool selection, and profile from the UI.
3. As a user, live inference reflects those bindings.

Current status:
- Green live on `workstation-1` for edit/rebind/inference on the active behavior.
- New-document execution remains a product gap because the chat UI does not yet expose a behavior picker.

### Scheduled Task Journey

1. As a user, I can edit and run a scheduled task from the UI.
2. As a user, validation errors are surfaced clearly.
3. As a user, run-now and failure inspection work through the same operator surface.

Current status:
- Green live on `workstation-1`.

## Shell-State Contract

The operator/config surface should follow the same shell-state methodology already established for chat:

1. UI emits explicit actions only.
2. Logic/controller owns side effects and transition application.
3. Projection is pure and derives visible state from `(shell state, store snapshot, peer status / client availability)`.
4. Snapshot refresh may hydrate or confirm a draft, but must not silently steal user selection or overwrite in-progress local edits that still match the current selection.
5. Transport/P2P health is diagnostic and recovery-related; it must not rewrite operator shell state.

This should become either:

- a generalized Lean-facing shell-state abstraction layered above `Proofs.ClientShell`, or
- a dedicated operator-shell spec plus Rust conformance tests.

The practical target is not “prove the whole UI.” The target is:

- explicit transition vocabulary
- snapshot non-mutation of user-owned shell state
- render purity
- conformance tests for the client-side transitions we care about

## Phase Plan

### Phase 1: Shell-State Audit and Coverage Matrix

- Inventory all operator sections and all editable fields.
- Map each document type to:
  - local GUI coverage
  - dedicated live CRUD coverage
  - helper-only coverage buried inside another journey
  - no coverage
- Identify which fields need live inference proof and which only need persistence/replication proof.
- Define the operator-shell transition contract in plain English before formalization.

Deliverable:
- Audit matrix document, test-gap checklist, and operator-shell contract notes.

### Phase 2: Operator Shell Conformance

- Decide whether to:
  - generalize `ClientShell` into a reusable shell-state abstraction, or
  - add a dedicated operator-shell layer with the same invariants.
- Add Rust conformance tests for operator shell transitions:
  - select deployment
  - select section
  - select entity
  - start new document
  - discard draft
  - apply success/failure
  - snapshot hydration
- Treat this as the client-side proof boundary for operator CRUD behavior.

Minimum required behavior:
- Snapshot refresh does not silently overwrite a matching local draft.
- Snapshot refresh does not steal selection.
- Render does not own operator-shell transitions.
- Transport state does not mutate operator selection/draft state.

Likely code areas:
- `crates/defra-agent/proofs/Proofs/ClientShell.lean`
- `crates/defra-agent-desktop/src/operator/*`
- `crates/defra-agent-desktop/tests/operator_view.rs`
- new operator conformance tests if needed

### Phase 3: Dedicated Live CRUD Journeys

- Replace the overloaded multi-agent switching test as the main source of document coverage.
- Add or extract one dedicated live journey per document type:
  - `Backends`
  - `ScheduledTasks`
  - `Behaviors`
  - `ToolSelections`
  - `InferenceProfiles`
- Prefer single-deployment canonical journeys unless multi-deployment isolation is the thing being tested.

Expected split:
- `operator_backend.rs`
- `operator_scheduled.rs`
- `operator_behavior.rs`
- `operator_tool_selection.rs`
- `operator_profile.rs`

### Phase 4: Multi-Deployment Isolation as a Separate Concern

- Shrink `desktop_app_live_multi_agent_server_switching_and_config_inference`.
- Keep only what actually needs two deployments:
  - deployment isolation
  - operator selection isolation
  - replicated config separation across agents
- Move backend/profile/tool-selection editing assertions out into their dedicated single-deployment journeys.

### Phase 5: Replication and Runtime Effect Verification

- Verify that config changes:
  - persist locally
  - replicate to the remote deployment
  - remain isolated across deployments
- For document types where live inference effect is observable:
  - prove the config is used by a real prompt
- For document types where direct runtime effect is not cleanly observable:
  - prove persistence, replication, and request binding rows instead of forcing a brittle prompt oracle

### Phase 6: Test Pruning and Final Audit Tightening

- Remove any old operator tests that are reduced to field-plumbing or internal draft assertions once stronger journeys exist.
- Keep only:
  - canonical local journeys
  - canonical live journeys
  - a few pure deterministic protocol/state tests

## Suggested Commit Series

1. `Document operator shell-state audit and coverage matrix`
2. `Add operator shell conformance tests`
3. `Extract live backend CRUD into dedicated journey` (already green)
4. `Extract live scheduled task CRUD into dedicated journey` (already green)
5. `Add live tool selection journey`
6. `Add live inference profile journey`
7. `Add live behavior journey`
8. `Reduce multi-agent switching to isolation-only coverage`
9. `Prune overlapping operator tests`

## Verification Standard

For each completed journey:

1. Local verification
   - `cargo test -p defra-agent-desktop --no-run`
   - focused journey tests

2. Live verification when behavior matters
   - use workstation backend env:
     - `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT=http://workstation-1:8000/v1`
     - `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL=MiniMax-M2.7-NVFP4`
     - `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_PROVIDER=openai-compatible`

3. Persistence / replication verification
   - local store reflects the change
   - remote deployment reflects the change
   - a later prompt proves the new config is live

## Immediate Next Step

Start with the missing dedicated journeys and conformance:

1. Add the operator-shell/conformance layer or equivalent Rust conformance tests.
2. Extract a dedicated live `ToolSelections` journey from the current `operator_config/*` helper path.
3. Extract a dedicated live `InferenceProfiles` journey from the same helper path.
4. After those are green, cut the multi-agent switching test down to deployment-isolation only.
