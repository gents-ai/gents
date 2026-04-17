# Desktop Journey Test Plan

## Goal

Shrink the desktop test surface toward a small set of high-value user journeys, while keeping only the deterministic logic tests that protect state-machine correctness and fast local iteration.

## Principles

- Treat live click-through journeys as the primary product contract.
- Keep local GUI journeys only when they materially speed up iteration on the same workflows.
- Keep pure unit tests only for deterministic rules that are hard to trust to live end-to-end coverage.
- Remove tests that mostly assert transient internal state, coverage bookkeeping, or field-by-field form plumbing without proving a real user journey.

## Canonical Journeys

1. Bootstrap and first connected chat
   - Desktop bootstraps from a running agent runtime.
   - The user can launch chat without manual refresh.
   - The user can switch between deployments and see isolated transcripts.

2. Multi-turn live chat
   - A user starts a conversation against live inference.
   - A follow-up in the same conversation succeeds.
   - Two conversations can be active concurrently and remain isolated.

3. Tool-loop live chat
   - A user asks for information that requires tool use.
   - The model makes the expected tool calls.
   - Tool artifacts render in transcript and operator views.

4. Operator config journey
   - A user edits operator config from the desktop.
   - The change persists locally and replicates remotely.
   - A later live request proves the new config is actually in use.

5. Multi-agent operator switching
   - A user switches between deployments in chat and operator.
   - Each deployment shows the correct behavior, backend, profile, and request timeline.
   - One deployment’s config changes do not leak into another deployment.

6. Peer repair and restart
   - A saved peer can reconnect after failure.
   - Repair and restart actions rebind the desktop cleanly.
   - Chat still works after transport recovery.

## Keep, Collapse, Delete

### Keep

- `src/app/tests/bootstrap.rs`
  - Canonical local bootstrap journeys.
- `src/app/tests/chat.rs`
  - Fast local chat journeys and transcript rendering flows.
- `src/app/tests/peers.rs`
  - Repair, restart, and P2P health flows.
- `src/app/tests/live/chat.rs`
  - Core live chat smoke and concurrent conversation flow.
- `src/app/tests/live/chat_followup.rs`
  - Live transcript artifact and retry/export flow.
- `src/app/tests/live/operator_switching.rs`
  - Canonical multi-agent switching and config isolation flow.
- `src/app/tests/live/operator_roundtrip.rs`
  - Canonical single-agent config editing flow.
- `src/app/tests/live/operator_scheduled.rs`
  - Scheduled task and failure journey.
- `src/app/tests/live/replication.rs`
  - Direct replication contract checks.

### Collapse

- Repeated chat and operator navigation/assertion code should move into shared journey helpers.
- Large journey files should prefer one high-signal test per journey over many narrow tests with duplicated setup.
- Field-heavy operator tests should assert the minimum set of edits needed to prove the workflow.

### Delete

- `src/app/tests/coverage.rs`
  - Removed. It tracked suite shape instead of protecting behavior.
- Live operator identity-field roundtrip coverage
  - Remove field-renaming tests that do not map to a canonical user journey.
- Local GUI tests that only prove selection bookkeeping or internal state repair
  - Remove or merge into stronger journey tests.

## Current Follow-On Order

1. Remove low-value test modules and narrow field-plumbing tests.
2. Consolidate repeated operator/chat journey steps into helpers.
3. Reduce the largest remaining journey files without lowering end-to-end coverage.
4. Revisit any remaining oversized pure logic test files only after the journey suite is stable.

## Status

- Removed `src/app/tests/coverage.rs`.
- Reduced `src/app/tests/chat.rs` to the stronger local chat journeys.
- Removed the live operator identity-field roundtrip test.
- Started extracting shared operator/chat journey helpers to shrink `bootstrap.rs` and `live/operator_switching.rs`.
