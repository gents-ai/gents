# Tauri Desktop Next Phase Plan

Status: execution started on `jack/tauri-desktop-next-phase` after
`../defra-agent-tauri-bridge-refactor` landed.

Current checkpoint:

- Phase 1 is implemented: egui code/test infra is removed and reusable desktop
  runtime code lives in `defra-agent-desktop-core`.
- Phase 2 is started: the fake Tauri UI journey was removed; the three-turn
  live Tauri UI journey is the desktop acceptance guard.
- Phase 3 is started: transcript/tool rendering and desktop-shell runtime
  helpers have been split out, and Rust timeline rendering is split out of the
  bridge snapshot builder.
- Phase 4 is started: the first behavior-config UI can edit the behavior
  display name and system prompt through the real Tauri bridge and observe the
  replicated row update.
- The live three-turn journey has passed against
  `http://100.73.235.38:8000/v1` using `MiniMax-M2.7-NVFP4`, but it is slow
  and should be run separately from quick config iteration.
- For quick config iteration, use `npm run test:live:config -- --inference-url
  http://100.73.235.38:8000/v1 --model-name MiniMax-M2.7-NVFP4`. Keep
  `npm run test:live:chat -- ...` for the slower three-turn chat journey.

## Goal

Move the desktop app fully onto the Tauri stack, delete the old egui app and
its test infrastructure, then build the config-management UI through
canonical live desktop journeys.

The product contract for desktop work is: a user can click through the real
desktop UI against a live node, submit real work to live inference when the
journey requires it, and observe the expected replicated documents and agent
behavior.

## Decisions

- Do not build static mocks for the config screens.
- Do not keep mock-heavy UI journey tests as a substitute for live desktop
  coverage.
- Keep only narrow deterministic tests where they protect pure logic that is
  hard to validate through a user journey.
- Treat live node tests, and live inference tests where relevant, as the
  acceptance path for desktop behavior.
- Remove old egui code and old egui test infrastructure in the same cleanup
  phase.
- Remove Tauri mock/fake UI flow tests when the live journey framework covers
  the same behavior.

## Phase 0: Land And Rebase

- Merge `../defra-agent-tauri-bridge-refactor`.
- Rebase this branch on the merged result.
- Capture the post-merge desktop baseline:
  - `cargo test`
  - `cd apps/desktop-tauri && npm test`
  - `cd apps/desktop-tauri && npm run test:live` with the required live env
    variables when available
- Do not start deletion work until the bridge runner and three-turn live UI
  journey pass on the merged code.

## Phase 1: Delete egui Without Losing The Desktop Core

Tauri currently uses the old `defra-agent-desktop` crate for non-UI runtime
pieces. Delete egui by separating reusable desktop core code from the old UI
before removing the old crate surface.

Work items:

- Create or rename a neutral runtime/client crate for reusable desktop core
  code.
- Move these non-UI pieces into that crate:
  - `ClientCore`
  - `ClientStore`
  - `DesktopPaths`
  - `PeerDirectory`
  - local runtime bootstrap and pairing code
  - desktop client mutation/query/store helpers still used by Tauri
- Point `apps/desktop-tauri/src-tauri` at the neutral crate.
- Delete old egui-only code:
  - egui app entrypoint
  - egui views
  - egui state modules
  - egui theme/fonts/assets
  - old egui app tests
  - old egui live/manage test harnesses
- Remove workspace dependencies that only existed for egui.
- Update scripts and docs that still refer to running or installing the old
  `defra-agent-desktop` app.

Acceptance:

- No `eframe`, `egui`, or `egui_commonmark` dependency remains.
- `rg "egui|eframe|egui_commonmark" Cargo.toml crates apps scripts` only
  finds historical docs, if we choose to keep any.
- Tauri still builds and can start its desktop core.
- The canonical live Tauri chat journey still passes.

## Phase 2: Prune Desktop Test Strategy

The old test surface mixed internal state checks, mocked UI flows, and live
journeys. Replace that with one explicit desktop testing model.

Work items:

- Promote the live Tauri driver into the canonical desktop test harness.
- Keep tests that click through the real UI and assert user-visible outcomes
  plus replicated document state.
- Remove egui tests with the egui code deletion.
- Remove fake-bridge/mock UI journey tests once equivalent live journeys exist.
- Keep small pure tests only for deterministic helpers such as formatting,
  routing decisions, or protocol projection rules.

Canonical journeys:

- Bootstrap and first connected chat.
- Three-round live conversation in one session.
- Tool-loop live chat that renders tool activity.
- Config edit roundtrip that proves the new config is actually used by a
  later request.
- Multi-deployment config isolation.
- Peer repair/restart followed by successful chat.

Acceptance:

- Desktop CI/test docs make it clear which tests are local deterministic
  checks and which are live acceptance journeys.
- No mock UI test is treated as acceptance for desktop behavior.
- Live journey failures produce enough diagnostics to debug replication,
  runtime, and UI state without rerunning under a debugger first.

## Phase 3: Clean The Tauri Code Before Feature Growth

Make the new app easier to modify before adding config screens.

Work items:

- Split large Rust bridge files by responsibility:
  - bootstrap snapshot
  - runtime/deployment snapshot
  - session snapshot
  - timeline rendering
  - tool detail rendering
  - command handlers
- Split `useDesktopShell` into focused hooks for:
  - snapshot/event sync
  - selected deployment/session/behavior state
  - chat send flow
  - client lifecycle and restart/health behavior
- Split chat UI components:
  - transcript
  - composer
  - header/title editing
  - tool groups
  - empty/running states
- Split CSS into stable domains or component-local files.

Acceptance:

- No active Tauri source file remains large enough to be a routine merge risk.
- Existing live journey behavior is unchanged.
- Config work can add new screens without editing one giant shell file.

## Phase 4: Build Config Management Testing First

Start with the first real config-management journey. Do not mock the screen;
write the live journey first, make it fail for the missing UI, then implement.

First journey:

- User opens desktop against a live node.
- User navigates from the selected deployment into config management.
- User edits behavior config.
- User saves the edit.
- The change persists and replicates.
- A later live request proves the edited config is actually in use.

Implementation order:

- Config workspace shell with deployment/agent context.
- Behavior list and editor.
- Save/apply path through Tauri bridge.
- Live test diagnostics for replicated config rows and later request behavior.
- User acceptance pass on the first screen before parallelizing.

Acceptance:

- The first config screen is accepted manually.
- Its live journey test is stable enough to run repeatedly.
- We have a reusable pattern for subsequent config screens.

## Phase 5: Expand Config Features

After the first behavior-config journey is accepted, split independent feature
work by config surface.

Suggested order:

- Inference backend editor.
- Inference profile editor.
- Tool selection editor.
- Task editor.
- Schedule editor.
- Manual task run once the backend path exists.
- Event trigger editor after the schema/runtime implementation is real rather
  than a placeholder.
- Request lineage/history view for manual, schedule, and event-triggered runs.

Each surface gets:

- One canonical live click-through journey.
- Focused deterministic tests only for non-UI pure logic.
- Manual acceptance after the live journey passes.

## Coordination Model

The first config screen should be done serially and high touch. That establishes
the UI structure, test harness conventions, diagnostics, and acceptance bar.

After that, work can parallelize by config surface as long as each branch owns a
separate UI/editor area and shares the same live-driver conventions. Manual
acceptance remains sequential.

## Immediate Next Step

Continue the Tauri code-size cleanup pass, then add the first failing live
config-management journey before implementing config editing UI.
