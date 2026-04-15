# defra-agent-desktop Chat Activity MVP (T6) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the desktop shell's Chat placeholder with the real MVP Chat activity: deployment-to-agent tree, conversation list, transcript, inline tool cards, composer, and turn-state chip backed by the desktop client core and observation store.

**Architecture:** The Chat activity consumes the T4 store snapshot and T5 mutation API. It remains intentionally plain in one area: markdown rendering and syntax highlighting are deferred to T7, so T6 should render raw text blocks cleanly but avoid taking a dependency on `egui_commonmark` or `syntect` yet.

**Tech Stack:** Rust 2021, `eframe`, `egui`, `defra-agent-protocol`, `chrono`, `tracing`.

**Reference spec:** `docs/superpowers/specs/2026-04-13-desktop-dashboard-design.md` (T6 row, Chat activity section, status bar, and first-launch experience).

---

## Execution environment

This ticket owns only the Chat activity. Do not start on markdown/syntax highlighting (T7), onboarding flows (T13), or command palette work (T14) in the same slice.

---

## File Structure

**New files:**

- `crates/defra-agent-desktop/src/views/chat/sidebar.rs`
- `crates/defra-agent-desktop/src/views/chat/transcript.rs`
- `crates/defra-agent-desktop/src/views/chat/composer.rs`
- `crates/defra-agent-desktop/src/views/chat/header.rs`
- `crates/defra-agent-desktop/tests/chat_view.rs`

**Modified files:**

- `crates/defra-agent-desktop/src/views/chat/mod.rs`
- `crates/defra-agent-desktop/src/views/mod.rs`
- `crates/defra-agent-desktop/src/app.rs`
- `crates/defra-agent-desktop/src/state.rs`

---

## Task 1: Add Chat-local UI state

**Files:**

- Modify: `crates/defra-agent-desktop/src/state.rs`

### Steps

- [ ] **Step 1: Add selection state**

Persist the minimum Chat selections:

- selected peer/deployment id
- selected `agent_did`
- selected `session_id`
- composer text
- selected behavior override, if any

- [ ] **Step 2: Add transient UI state**

Track view-local state such as:

- expanded tool-card ids
- transcript scroll anchoring
- last submission error

Keep this state separate from `ClientStore`, which remains the replicated read model.

---

## Task 2: Implement the left sidebar

**Files:**

- Create: `crates/defra-agent-desktop/src/views/chat/sidebar.rs`

### Steps

- [ ] **Step 1: Render the deployment-to-agent tree**

Build the `Deployments` section from:

- peer directory records
- replicated agents observed in the store

Tree grouping is display-only. Selection routes by `agent_did`.

- [ ] **Step 2: Render conversations grouped by recency**

Build the `Conversations` section grouped into:

- Today
- Yesterday
- Earlier

Each row should show title, short metadata, and a relative timestamp.

- [ ] **Step 3: Add empty states**

If there are:

- no peers, show the Add-deployment empty state shell
- no conversations for the selected agent, show the Create-conversation nudge

The actual onboarding dialog arrives later; the empty state must still exist now.

---

## Task 3: Implement the transcript and header

**Files:**

- Create: `crates/defra-agent-desktop/src/views/chat/transcript.rs`
- Create: `crates/defra-agent-desktop/src/views/chat/header.rs`

### Steps

- [ ] **Step 1: Build the chat header**

Render:

- breadcrumb `deployment / agent / conversation`
- turn-state badge
- placeholder Retry and Export actions

Retry/Export may be disabled if the behavior is not wired yet, but the header structure must be present.

- [ ] **Step 2: Build the transcript body**

Render ordered transcript content from the store:

- user and assistant messages
- inline tool cards for tool-call rows
- plain-text reasoning block if present on the latest response

Reasoning does not need collapsible chrome yet; T7 owns that refinement.

- [ ] **Step 3: Add the turn-state chip**

Above the composer, render a status chip derived from:

- `store.derive_turn(session_id)`

Use the five `ClientTurnState` values from the protocol crate; do not invent new states.

---

## Task 4: Implement the composer

**Files:**

- Create: `crates/defra-agent-desktop/src/views/chat/composer.rs`

### Steps

- [ ] **Step 1: Render the composer layout**

Include:

- multiline text input
- behavior chip
- tool-selection chip
- `Cmd+Enter` hint
- send button

- [ ] **Step 2: Wire submission**

On submit:

- create a conversation if needed
- call `ClientCore::submit_request`
- keep selection on the active session
- clear the composer text on success

- [ ] **Step 3: Disable submission while a turn is active**

The send button and submit shortcut should be disabled when the selected session has a non-terminal client turn state.

---

## Task 5: Integrate Chat into the shell

**Files:**

- Modify: `crates/defra-agent-desktop/src/app.rs`
- Modify: `crates/defra-agent-desktop/src/views/mod.rs`

### Steps

- [ ] **Step 1: Replace the Chat placeholder**

Wire the new Chat view into the root app while leaving the other activities as placeholders.

- [ ] **Step 2: Connect status-bar data**

Use real Chat-adjacent values in the status bar where available, for example:

- selected agent runtime state
- current principal DID
- peer counts

If lag/error metrics are not ready yet, keep placeholder values explicit rather than implying they are live.

---

## Task 6: Verify the Chat MVP

**Files:**

- Create: `crates/defra-agent-desktop/tests/chat_view.rs`

### Steps

- [ ] **Step 1: Add pure UI-logic tests**

Cover:

- conversation grouping into Today/Yesterday/Earlier
- send-disabled logic for non-terminal turns
- deployment-tree grouping by selected peer and `agent_did`

- [ ] **Step 2: Add focused integration coverage**

Use a temp embedded node to prove that:

- creating a conversation and submitting a request makes the Chat sidebar and transcript state coherent after store refresh
- the turn-state chip reflects the protocol crate's derivation

- [ ] **Step 3: Compile and test**

Run:

- `cargo check -p defra-agent-desktop`
- `cargo test -p defra-agent-desktop chat_view`

- [ ] **Step 4: Manual smoke test**

Run: `cargo run -p defra-agent-desktop`

Expected: Chat is the first activity with real data and write paths; other activities remain placeholders.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-desktop/
git commit -m "Add defra-agent-desktop chat MVP"
```
