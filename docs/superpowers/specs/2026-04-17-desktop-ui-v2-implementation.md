# Desktop UI V2 Implementation Plan

This plan turns the approved V2 design into an incremental rewrite against the
current `defra-agent-desktop` codebase.

The goal is not a one-shot redesign. The goal is to move the running desktop
app toward the approved interaction model while preserving the working client,
replication, and mutation plumbing.

## Target model

V2 has two user-facing surfaces:

- `Chat`
- `Manage deployment`

Chat is the persistent home. Management is entered from the selected
deployment and should feel scoped to that deployment, not like a separate app.

Desktop-local diagnostics and legacy peer setup survive only as supporting
flows or debug surfaces until they are either folded into management or
deleted.

## Current code seams

The existing implementation still reflects the older four-activity shell:

- routing and shell state:
  - [state/activity.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/state/activity.rs)
  - [state/shell.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/state/shell.rs)
  - [app/shell_actions.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/app/shell_actions.rs)
- global shell/sidebar composition:
  - [views/shell.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/shell.rs)
  - [views/mod.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/mod.rs)
  - [app/panels.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/app/panels.rs)
- chat composition:
  - [views/chat/container.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/chat/container.rs)
  - [views/chat/sidebar/mod.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/chat/sidebar/mod.rs)
  - [views/chat/header.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/chat/header.rs)
  - [views/chat/composer.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/chat/composer.rs)
  - [views/chat/transcript/mod.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/chat/transcript/mod.rs)
- operator/management composition:
  - [views/operator/mod.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/operator/mod.rs)
  - [views/operator/sidebar.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/operator/sidebar.rs)
  - [views/operator/rail.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/operator/rail.rs)
- peer setup / legacy flows:
  - [views/peers/mod.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/peers/mod.rs)
  - [views/peers/detail/onboarding.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/peers/detail/onboarding.rs)

## Rewrite phases

## Phase 1: Chat-first shell cut

Goal: make the running app feel like Chat is home and management is contextual.

Changes:

- remove app-level sidebar chrome from [views/shell.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/shell.rs):
  - desktop DID block
  - chat / peers / logs activity buttons
- keep deployments as the top-level selector shared by Chat and Manage
- rename contextual deployment action from `Configure` to `Manage`
- ensure Manage has an explicit return path back to Chat
- keep legacy activities alive internally only where they are still needed

Expected outcome:

- the visible shell matches the approved mock more closely
- the user no longer perceives the app as four competing top-level tools

## Phase 2: Chat hierarchy cleanup

Goal: make Chat visually and behaviorally express:

`deployment -> behavior -> conversation`

Changes:

- merge deployment selection and management affordance into the chat selector
- keep `New conversation` inside the conversations section
- make conversation title editing first-class
- remove header actions that do not belong to the common path
- show turn progress inline under the last visible bubble
- fix retry transcript fallback in
  [views/chat/transcript/mod.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/chat/transcript/mod.rs)
  so retries do not duplicate the user prompt

Expected outcome:

- chat reads as one coherent flow
- the sidebar expresses the real conversation hierarchy
- state is visible where the user is looking

## Phase 3: Manage deployment workspace

Goal: transform the current operator surface into the approved full-page
management workspace.

Changes:

- rename `Operator` language to `Manage deployment`
- move section navigation to top tabs instead of a sidebar-first experience
- keep entity pickers local to the selected tab
- reserve the right rail for diagnostics only
- merge useful peer health and replication diagnostics into management
- remove mystery actions in favor of explicit status panels

Primary files:

- [views/operator/mod.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/operator/mod.rs)
- [views/operator/sidebar.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/operator/sidebar.rs)
- [views/operator/entity_list.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/operator/entity_list.rs)
- [views/operator/rail.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/operator/rail.rs)

## Phase 4: Onboarding and peer setup

Goal: stop treating peer/deployment setup as a cramped sidebar toggle flow.

Changes:

- move add-deployment setup into a primary-pane onboarding/editor flow
- preserve current client/pairing logic while replacing the presentation
- route first successful connection into Chat with the new deployment selected

Primary files:

- [views/peers/detail/onboarding.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/peers/detail/onboarding.rs)
- [views/peers/forms.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/views/peers/forms.rs)
- [app/tests/first_launch.rs](/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent-desktop/src/app/tests/first_launch.rs)

## Phase 5: Remove obsolete surfaces

Goal: delete the parts of the shell that no longer match the approved product.

Changes:

- collapse `Activity` away from four peer surfaces toward Chat + Manage
- delete standalone Logs surface once any needed diagnostics are folded into
  status or management
- delete or inline legacy Peers-only presentation where no longer needed
- simplify tests to the new surface model

## Test strategy

After each phase, run focused desktop tests rather than waiting for the full
rewrite to land.

Minimum set:

- `cargo test -p defra-agent-desktop --test chat_view`
- `cargo test -p defra-agent-desktop --test operator_view`
- `cargo test -p defra-agent-desktop --test client_store`

Add or update live tests when navigation semantics change:

- `cargo test -p defra-agent-desktop app::tests::chat`
- `cargo test -p defra-agent-desktop app::tests::operator`

## Immediate next implementation step

Start with Phase 1.

That gives the product the right top-level shape before deeper component
rewrites. The old shell chrome is currently exaggerating the sense that the UI
is fragmented, so removing that chrome is the highest-leverage first cut.
