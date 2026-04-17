# Desktop Readability Refactor Plan

## Goal

Reduce file size, sharpen module boundaries, and make the desktop codebase safer to work in without changing behavior.

## Rules

- No new feature work in oversized catch-all files.
- Prefer extraction along existing state and workflow boundaries.
- Keep render logic separate from projection and side effects.
- Keep P2P/runtime side effects separate from store/query code.
- Aim for `250-400` lines for most files and treat `500` as a hard warning threshold.
- `mod.rs` files should become dispatch/re-export shells, not implementation dumps.

## Active Order

1. Split `src/client/core.rs`
2. Split `src/client/mutations.rs`
3. Split `src/views/operator/mod.rs`
4. Split `src/views/peers/mod.rs`
5. Split `src/views/chat/transcript.rs`
6. Split `src/app.rs`
7. Split the desktop test harness under `src/app/tests`

## Status

### Phase 1: `src/client/core.rs`

- Extract startup/bootstrap into `src/client/core/bootstrap.rs`
- Extract P2P operation wrappers into `src/client/core/p2p_ops.rs`
- Extract supervisor/repair/health logic into `src/client/core/supervisor.rs`
- Extract write/mutation methods into `src/client/core/writes.rs`
- Move unit tests into `src/client/core/tests.rs`
- Leave `src/client/core.rs` as the public spine for shared types and simple accessors

Status: completed

### Phase 2: `src/client/mutations.rs`

- Split into:
  - `src/client/mutations/chat.rs`
  - `src/client/mutations/peers.rs`
  - `src/client/mutations/operator.rs`
  - `src/client/mutations/graphql.rs`

Status: completed

### Phase 3: `src/views/operator/mod.rs`

- Split into:
  - `src/views/operator/mod.rs`
  - `src/views/operator/sidebar.rs`
  - `src/views/operator/rail.rs`
  - `src/views/operator/runtime.rs`
  - `src/views/operator/request_timeline.rs`
  - `src/views/operator/recent_failures.rs`
  - `src/views/operator/drafts.rs`
  - `src/views/operator/editors.rs`
  - `src/views/operator/shared.rs`

Status: completed

### Phase 4: `src/views/peers/mod.rs`

- Split into:
  - `src/views/peers/mod.rs`
  - `src/views/peers/list.rs`
  - `src/views/peers/detail.rs`
  - `src/views/peers/forms.rs`
  - `src/views/peers/actions.rs`
  - `src/views/peers/shared.rs`

Status: completed

### Phase 5: `src/views/chat/transcript.rs`

- Split into:
  - `src/views/chat/transcript/mod.rs`
  - `src/views/chat/transcript/messages.rs`
  - `src/views/chat/transcript/tool_cards.rs`
  - `src/views/chat/transcript/reasoning_cards.rs`
  - `src/views/chat/transcript/modal.rs`
  - `src/views/chat/transcript/markdown.rs`

Status: completed

### Phase 6: `src/app.rs`

- Split into:
  - `src/app/mod.rs`
  - `src/app/client_binding.rs`
  - `src/app/p2p_restart.rs`
  - `src/app/shell_actions.rs`
  - `src/app/bootstrap.rs`

### Phase 7: Test Harness

- Split into:
  - `src/app/tests/support/driver.rs`
  - `src/app/tests/support/wait.rs`
  - `src/app/tests/support/fixture.rs`
  - `src/app/tests/support/live_backend.rs`
  - `src/app/tests/live/chat.rs`
  - `src/app/tests/live/operator.rs`
  - `src/app/tests/live/peers.rs`
