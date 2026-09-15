# Chat responsiveness acceptance

## Retained benchmark

Run the isolated deterministic workload from `apps/gents-desktop`:

```bash
npm run perf:mobile -- --runs=5
```

The `mobile-interactions-v1` fixture opens a 600-turn session, deliberately loads
five 40-turn pages, and types 160 characters in the canonical composer while the
session is streaming and again after it becomes idle. The first browser-process
sample is reported as cold; the four warm samples form the distributions. The
runner records every input event through the next paint, React Profiler work, DOM
size, long tasks, and bridge calls.

The responsive budgets are:

- input event to next paint p95: at most 50 ms in every sample;
- total React render work: at most 12 ms per typed character in every sample;
- bridge calls during either typing burst: zero;
- mounted transcript turns during either typing burst: exactly 200.

## Reference evidence

Measured on 2026-09-15 with headless Chromium 149.0.7827.55 at 390x844, Node
v25.8.2, Vite's development transform, and an arm64 macOS 25.5.0 host with 18
logical CPUs and 128 GiB memory. Both runs used five samples with the first
sample excluded from the warm distributions.

| Long-transcript workload | Warm median before | Warm median after | Change |
| --- | ---: | ---: | ---: |
| Streaming burst elapsed | 9,491 ms | 1,643 ms | -82.7% |
| Streaming input-to-paint p95 | 149.5 ms | 30.9 ms | -79.3% |
| Streaming React work | 7,506.6 ms | 784.6 ms | -89.5% |
| Idle burst elapsed | 9,916 ms | 1,423 ms | -85.6% |
| Idle input-to-paint p95 | 152.0 ms | 24.6 ms | -83.8% |
| Idle React work | 7,879.6 ms | 648.4 ms | -91.8% |

The optimized run's worst sample had 38.9 ms input-to-paint p95 and 4.92 ms of
React work per character. Neither workload made a bridge call, before or after.
No long tasks occurred in the optimized warm samples.

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
