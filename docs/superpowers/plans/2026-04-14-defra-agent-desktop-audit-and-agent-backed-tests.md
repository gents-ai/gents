# defra-agent desktop audit and agent-backed test plan

## Goal

Audit the desktop work that landed in T2-T13 with interaction-driven tests instead of paint-only snapshots, then add a second lane that runs against a real `defra-agent` runtime so the UI is exercised with actual replicated traffic.

## Problems to solve

1. Current desktop tests are mostly render assertions. They prove that views paint, but they do not prove that buttons, selectors, text inputs, or cross-view flows actually work.
2. Most tests seed rows directly through `ClientCore` mutations. That is useful, but it does not validate the runtime path where a real agent watches requests, produces responses, updates `AgentRuntime`, and drives the UI with live state changes.
3. We need a clear split between deterministic CI-safe coverage and env-gated live-inference coverage.

## Audit tracks

### Track A: in-process interaction audit

Add a headless `DesktopApp` driver that:

- runs the actual app frame loop through `egui::Context::run_ui`
- feeds `egui::RawInput` pointer and keyboard events across frames
- clicks widgets by visible text or stable test ids
- types into `TextEdit` fields
- asserts on post-click app state plus replicated store changes

This is the fast default audit lane. It should cover:

- first-launch onboarding
- activity switching
- peers add/remove flow
- chat conversation selection and first-conversation creation
- chat send flow
- operator section switching and selected-entity changes
- logs filter chips

### Track B: real runtime fixture

Add a reusable fixture that:

- boots an embedded node for the desktop app
- provisions `AgentPrincipal`, `InferenceBackend`, `ToolSelection`, `InferenceProfile`, and `AgentBehavior`
- starts a real `defra-agent` runtime with `tokio::spawn(agent.run(shutdown_rx))`
- waits for `AgentRuntime.process_state = ready`
- submits requests through the same document path the desktop already observes

This lane should first use a deterministic mock OpenAI-compatible endpoint that supports:

- `GET /v1/models`
- `POST /v1/chat/completions`

That gives us real request/response/runtime churn without external dependencies.

### Track C: env-gated live inference smoke

Add ignored tests that switch the runtime fixture from the mock endpoint to a live inference backend when env vars are present.

Suggested env surface:

- `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_ENDPOINT`
- `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_MODEL`
- `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_PROVIDER`
- `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY`
- `DEFRA_AGENT_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR`

This lane should stay small:

- boot runtime
- send one request
- wait for a completed response
- verify Chat, Operator timeline, and Logs show the resulting activity

## Implementation sequence

### Phase 1: harness

1. Add a `DesktopApp` test driver with frame stepping and synthetic `RawInput`.
2. Add helpers for pointer click, keyboard text entry, shortcut dispatch, and rendered-text hit lookup.
3. Move the current `render_once` helper behind the driver so old tests and new click-through tests share one path.

### Phase 2: deterministic audit coverage

1. Add onboarding click-through coverage:
   - blank launch redirects to Peers
   - copy DID action emits feedback
   - first deployment form accepts input and saves a peer
2. Add Chat click-through coverage:
   - switch to Chat
   - create first conversation
   - type into composer
   - send request
3. Add Operator/Logs navigation coverage:
   - switch sections
   - select rows
   - toggle Logs filter chips

### Phase 3: real runtime fixture

1. Extract the existing `MockModelEndpoint` / `MockCompletionEndpoint` patterns from `defra-agent` runtime and scheduler tests into a reusable desktop-side fixture.
2. Start a real runtime against the desktop node and verify:
   - `AgentRuntime` transitions to `ready`
   - submitted requests progress to responses
   - Chat transcript updates without direct row seeding
   - Logs receives runtime/turn events

### Phase 4: live inference smoke

1. Add ignored env-gated tests for a real inference endpoint.
2. Keep assertions coarse and resilient.
3. Do not run by default in CI.

## Success criteria

- every activity has at least one interaction-driven audit test
- the audit harness can click visible controls and type into fields
- at least one desktop test uses a real running `defra-agent` runtime instead of only seeded rows
- live inference smoke is available but opt-in

## Non-goals

- replacing all snapshot-style tests immediately
- OS-level automation on this pass
- broad fuzzing or visual diffing

OS-level click automation can come later if the in-process harness still misses failures we care about.
