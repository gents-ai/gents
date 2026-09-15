# Chat responsiveness acceptance

## Retained benchmark

Run the isolated deterministic workload from `apps/gents-desktop`:

```bash
npm run perf:mobile -- --runs=5 --enforce-responsive-budgets
```

The runner always records the responsive timing budgets. By default, only
deterministic assertions with `policy: hard` affect its exit code, matching the
CI lane's shape-contract role. Use `--enforce-responsive-budgets` for a controlled
local acceptance run where timing failures should fail the command.

The `mobile-interactions-v1` fixture opens a 600-turn session, deliberately loads
five 40-turn pages, and types 160 characters in the canonical composer in three
distinct conditions: during an active turn with no newly delivered deltas, while
50 streamed updates are delivered concurrently with typing, and after the turn
becomes idle. The first browser-process sample is reported as cold; the four warm
samples form the distributions. The runner records every input event through the
next paint, React Profiler work, DOM size, long tasks, and bridge calls.

The responsive budgets are:

- input event to next paint p95: at most 50 ms in every sample;
- total React render work: at most 12 ms per typed character in every sample;
- bridge calls during active-turn/no-delta and idle typing bursts: zero;
- concurrent streaming typing observes one or more bounded live-delta reads and
  makes no desktop snapshots, session snapshots, or submit calls;
- mounted transcript turns during every typing burst: exactly 200.

## Reference evidence

Measured on 2026-09-15 with headless Chromium 149.0.7827.55 at 390x844, Node
v25.8.2, Vite's development transform, and an arm64 macOS 25.5.0 host with 18
logical CPUs and 128 GiB memory. Both runs used five samples with the first
sample excluded from the warm distributions.

| Long-transcript workload                | Warm median before | Warm median after | Change |
| --------------------------------------- | -----------------: | ----------------: | -----: |
| Active-turn/no-delta burst elapsed      |           9,491 ms |          1,643 ms | -82.7% |
| Active-turn/no-delta input-to-paint p95 |           149.5 ms |           30.9 ms | -79.3% |
| Active-turn/no-delta React work         |         7,506.6 ms |          784.6 ms | -89.5% |
| Idle burst elapsed                      |           9,916 ms |          1,423 ms | -85.6% |
| Idle input-to-paint p95                 |           152.0 ms |           24.6 ms | -83.8% |
| Idle React work                         |         7,879.6 ms |          648.4 ms | -91.8% |

These historical figures predate the concurrent-delta scenario. The optimized
run's worst sample across the active-turn/no-delta and idle workloads had 38.9 ms
input-to-paint p95 and 4.92 ms of React work per character. Neither historical
workload made a bridge call, before or after. No long tasks occurred in the
optimized warm samples. Concurrent-delta measurements must come from a new
artifact and should not be inferred from this table.

### Concurrent-delta review evidence

The final input-coupled review follow-up ran on 2026-09-15 with five samples and
`--enforce-responsive-budgets`. It used baseline commit
`67e67beada6142a0b0a9952add94c3edc5990304` on
`perf/onboarding-chat-responsiveness`, plus a dirty review tree identified by
SHA-256 fingerprint
`8c832d88ac14b0934e060355e83c01eb996161403b7f4e710c372e98ae720020`.
The local artifact is
`/tmp/gents-chat-input-coupled-final-5/mobile-performance.json`; preserve or
publish it separately when durable provenance is required.

Every sample captured all 160 inputs and interleaved exactly 50 streamed updates
with 50 live-delta bridge reads. The four-sample warm distribution had a 25.20 ms
median input-to-paint p95 and a 25.64 ms worst sample. Worst React work across all
five samples was 6.28 ms per character. Each concurrent burst transferred 16,479
bridge response bytes, with a 331-byte largest response, and made no snapshot or
submit call. All hard assertions and opt-in responsive budgets passed. This is
dirty-tree review evidence, not a clean-baseline replacement for the historical
before/after table above.

## Native acceptance checklist

The coordinator and user should run this against the shared native acceptance
build with its isolated test home. Do not reuse a personal conversation or
account transcript.

- [ ] Open the 200-or-more-turn acceptance session and confirm all loaded content
      remains visible.
- [ ] Type and erase a paragraph of at least 160 characters while the session is
      idle; confirm the text tracks input without visible stalls.
- [ ] Start a streamed response and type another 160-character paragraph while
      tokens arrive; confirm both the draft and streamed tail continue updating.
- [ ] Start IME composition, press Enter while composing, and confirm it neither
      submits nor accepts a slash-skill suggestion. Finish composition and submit
      normally.
- [ ] Put distinct unsent drafts in two sessions and in a fresh Setup session;
      switch among them and confirm each draft returns only in its owning context.
- [ ] Scroll away from the transcript tip while streaming and confirm the reader's
      position stays fixed. Return to the tip and confirm following relocks.
- [ ] Load at least three older pages and confirm the same visible message stays in
      place after each prepend.
- [ ] Cancel a streaming request and confirm the interruption state remains
      readable and the composer becomes available again.
- [ ] Exercise a retryable failure and confirm the error text is readable, Retry
      remains available, and retry starts through the existing request owner.
- [ ] Confirm the run did not restart the native app, change accounts, or touch the
      user's live conversation.

State semantics and provider input are unchanged, so this work requires no Lean
proof update.
