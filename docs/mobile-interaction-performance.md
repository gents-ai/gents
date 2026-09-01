# Mobile interaction performance baseline

This track establishes measurement infrastructure and initial structural
budgets for the Gents iOS surface. It does not change runtime/provider
semantics, add a client cache, or treat a fast but stale state as success.

## Baseline identity and comparability

The source baseline is commit
`3b3e4dc431916661e94d3d6ae5d06ad0b8998e0e` on `main`. The measurements below
were captured from the optimization stages described below, all rooted at that
commit, on 2026-08-25 in America/Los_Angeles.

The database-owned projection-controller extension was developed from current
`main` commit `8b0b6eb731ab1404a1e761aec57f592951c7a5c1` (`feat: add
owner-scoped mobile mailbox (#1205)`) on 2026-08-26. Its final dirty-tree
measurement fingerprint is
`e86db8ae4535aa9923f2ab3bea3a0f076b521c1d7bfb22e7ca8db482a8c72ccd`.
Its deterministic structural results are reported separately; no wall-clock or
memory trend is claimed against the older measurement root.

| Property      | Browser evidence class                                                            | Native simulator class                                         |
| ------------- | --------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| Host          | MacBook Pro `Mac17,6`, Apple M5 Max, 18 logical cores, 128 GiB                    | same                                                           |
| Host OS       | macOS 26.5.1 (25F80), Darwin 25.5.0                                               | same                                                           |
| Toolchain     | Node 25.8.2, npm 11.13.0, Chromium 149.0.7827.55                                  | Xcode 26.6 (17F113), rustc 1.97.1                              |
| View/device   | Chromium viewport 390x844                                                         | iPhone 17 Pro simulator `3AA7C43D-D679-4895-B679-89FA5445F7A2` |
| Runtime       | browser engine above                                                              | iOS 26.5 (23F77)                                               |
| Build profile | Vite development transform, headless Chromium                                     | Tauri iOS debug, `aarch64-sim`                                 |
| Data state    | first browser-process sample recorded separately; four fresh-context warm samples | reset-data and keep-data samples are separate classes          |

Numbers are comparable only when the commit/fixture version, engine or device,
OS/runtime, build profile, and cold/warm class match. The browser lane is a
repeatable projection/rendering proxy, not an iOS launch measurement. The first
browser-process sample is `n=1` evidence, not a distribution or trend.
Simulator values likewise must not be compared with physical-device values.

## Durable fixture and scenario contract

The versioned `mobile-interactions-v1` fixture contains 120 session index rows,
a one-item short session, a 600-item large transcript, a 40-item transcript page,
50 streamed updates, a 25-event synchronous update burst, and ten repeated
short/large navigation cycles. IDs, text lengths, ordering, and counts are
deterministic. Changing the shape requires a new fixture ID and a fresh baseline.

| Required scenario      | Current boundary and evidence                                                                          | Current limitation                                                                                       |
| ---------------------- | ------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------- |
| Cold launch to shell   | native process launch to `shell-interactive`; browser navigation proxy reported separately             | only native is product launch evidence                                                                   |
| Paired launch to index | native launch to visible `.conversation-list`; browser agent selection to 120 visible rows             | native seeded long index is future work                                                                  |
| Cached short session   | click to visible known local message                                                                   | browser projection fixture today                                                                         |
| Large local tip        | click to visible last item of 600; record the bounded bridge page and mounted rows                     | bridge projection CPU still starts from the observed in-memory store; native durable seed is future work |
| Page older             | query-level DefraDB cursor page plus bridge merge; assert the prior expensive DOM node remains mounted | deterministic browser UI merging is paired with real-node Rust integration coverage                      |
| Sustained stream       | 50 sequential update events, each observed in the transcript                                           | deterministic adapter models event/refresh pressure, not provider token cadence                          |
| Foreground recovery    | native correlated status stream plus observer counters; browser projects stalled then healthy          | real iOS suspend/network repair awaits #1143/#893                                                        |
| Remote hydration       | reserved fixture contract below                                                                        | intentionally not implemented on this branch                                                             |

The future `mobile-hydration-v1` fixture should seed 600 transcript documents on
the paired server while the phone has only the session index row. Opening it must
record one stable correlation ID at request write, first accepted merge,
`served_doc_count`, visible first page, and terminal complete/failed state. It
must count request/repair attempts, merged document IDs, duplicate arrivals,
and bytes. The #1154 hydration foundation is now the integration base, so this
worktree calls its session-focus extension point but deliberately does not add
the on-demand fixture or reproduce the hydration lifecycle. Foreground/network
repair still depends on #1143.

The final compatibility base inspected on 2026-08-25 was `origin/main` at
`c9ccf9ba`, which includes the squashed #1154 hydration foundation as
`cf15aac4`. This branch was rebased onto that main head before final merge
validation. #1154 owns the hydration schema, request write, sweep, reconcile,
proofs, and live E2E. It also changes `client/core.rs`,
`client/core/supervisor.rs`, `client/observe.rs`, and the bridge chat commands,
which overlap this track's store revision and session-read seams. The rebased
implementation preserves that ownership: a hydration request or accepted merge
advances the authoritative reconcile revision, while an `AgentResponse`-only
stream update may retain the response fast path. Scenario 8 remains a future
extension of the landed live E2E and its real document lifecycle.

## Instrumentation and ownership

`npm run perf:mobile` wraps the existing adapter seam in measurement mode. It
uses monotonic clocks, a React `Profiler`, browser task/heap counters, long-task
and DOM-mutation observers, and byte counts of serialized bridge arguments and
results. The wrapper is installed only for the performance fixture. Production
code pays no instrumentation cost.

The performance fixture disables the production 1.5-second active-session poll
through the existing test timing seam. This keeps the event-driven stream
budget structural on slow and fast runners: an unrelated timer cannot add a
session snapshot merely because one sample crosses 1.5 seconds. The production
fallback remains enabled and is covered outside the measurement fixture.

The native lane accepts `--measure`. Debug-only driver statuses include a
bounded correlation ID, application `performance.now()` boundary, DOM counts,
and `desktop_observer_metrics`. The Rust bridge validates a 32 KiB record cap,
atomically writes current status, appends JSONL events, and emits a bounded
`tracing` event. The runner samples simulator RSS, CPU percentage, and process
CPU time every 250 ms. These owners make the artifact actionable:

| Metric                        | Instrument/owner                                                                                                                      |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| elapsed boundary              | monotonic runner/application clock; startup/UI owner                                                                                  |
| commit duration and rerenders | React profiler; chat/shell owner                                                                                                      |
| bridge bytes and frequency    | adapter call ledger/native event record; bridge/projection owner                                                                      |
| DefraDB query/merge proxies   | observer fetched-doc, scope-reload, failure, and drop-recovery counters; desktop-core owner                                           |
| response merge allocation     | observer in-place and copy-on-write response merge counters; desktop-core owner                                                       |
| materialized/rendered rows    | fixture snapshot plus DOM counts; projection/chat owner                                                                               |
| memory high-water/growth      | simulator RSS and browser JS heap; native/UI owner respectively                                                                       |
| CPU/energy proxy              | simulator process CPU and Chromium task duration/long tasks; no energy claim                                                          |
| reconnect/repair              | observer drop-recovery counters and time to visible state; full pairing retry counters remain blocked on #1144; supervisor/sync owner |

Artifacts contain exact environment metadata, fixture metadata, every raw
sample, median/p95 distributions, and deterministic assertion results. Generate
them with:

```bash
npm run build:packages
cd apps/gents-desktop
npm run perf:mobile -- \
  --runs=5 --output=test-results/mobile-performance/local

# Requires Xcode, XcodeGen, an issuer, and an available simulator.
# The issuer home must be initialized, have an AgentNetwork, and be served by a
# running P2P-enabled `gents server`. The runner prefers target/release/gents and
# falls back to target/debug/gents.
npm run test:ui:ios:e2e -- \
  --runs=3 --measure --artifacts=test-results/mobile-performance/ios
```

The browser command is part of the Linux desktop-browser CI shard with three
samples, where only structural assertions fail. The weekday macOS QA sweep
captures five-sample JSON/Markdown evidence. The native command is the smallest
practical simulator smoke lane: it builds, installs, pairs, sends one prompt,
waits on rendered states rather than sleeps, and records launch/index/round-trip
boundaries. It remains manual pending iOS CI issue #890.

Device-only work remains: MetricKit/energy-log evidence, thermal state, physical
resident memory behavior, jetsam, radio/network transitions, lock/background
suspension, and App Store build performance. Simulator CPU is only a proxy.

### Native validation result

On the simulator environment recorded above, the current `aarch64-sim` debug
archive built, installed, and reached the correlated `shell-interactive` and
`pairing` boundaries. That exercise found and fixed three harness defects: raw
Tauri command names were not qualified with the bridge plugin, a transient
near-zero WKWebView visual viewport could collapse the shell, and a global
pairing error did not terminate the observable-state wait. The command now
falls back to an available debug issuer CLI, rejects unusable viewport samples,
resamples on page/window lifecycle events, and turns bridge/global failures into
an explicit `failed` boundary.

The predecessor round trip then established P2P connectivity but truthfully
rejected its readiness acknowledgement after the current signed endpoint
differed from the endpoint bound into that acknowledgement. That historical
result is not a successful performance sample. No
native wall-clock, RSS, CPU, or observer distribution is published. The next
native acceptance run must first make readiness stable across signed endpoint
publication; #890 should run the corrected harness, while the pairing/sync
owner should close the endpoint race without relaxing verification.

### Live inference acceptance

Acceptance uses the real desktop bridge, two embedded DefraDB nodes, P2P
replication, request persistence, the runtime completion loop, and the rendered
React transcript. On 2026-08-26 both live lanes passed:

- `npm run test:ui:live:e2e` completed a browser-to-runtime turn against the
  deterministic local OpenAI-compatible inference server, including the
  production one-query transcript tip assertion, with two visible transcript
  rows and no browser errors;
- `npm run test:live:chat` completed three turns in one durable session against
  the configured real OpenAI-compatible GLM-5.2 endpoint, ending with 10
  materialized messages and two tool calls.

These are correctness witnesses, not wall-clock baselines: the local mock and
real provider are different inference classes and their single-run durations
must not be compared. The acceptance work found and fixed three harness defects:
manual live-fixture replicators were still being reconciled away by the normal
supervisor, both live suites expected a conversation-level Operations drawer
that the product contract intentionally removed, and the browser HTTP adapter
lacked the tool-hold endpoints used by the rendered shell. The supervisor
change is gated by the existing `install_replicators_on_bootstrap = false`
fixture option; default product behavior is unchanged.

The deterministic live lane also asserts the production transcript query path.
The final path fetches the bounded message and tool windows in one DefraDB
GraphQL evaluation at the tip (plus one cursor-validation query for older
pages), reports a 41-message lookahead and 321-tool-row overscan limit, and
materializes only complete sequence groups. The atomic read prevents a live tip
insert from producing an orphan tool group between two page queries. Those
counts are attached to the live smoke JSON/Markdown artifact.

## Measured browser baseline

### Pre-optimization baseline

This table is the warm, fresh-context distribution (`n=4`) after excluding the
first browser-process sample. Values are wall-clock evidence only.

| Scenario                                       | median ms |  p95 ms | median bridge response | median React commit work |
| ---------------------------------------------- | --------: | ------: | ---------------------: | -----------------------: |
| Cold-launch shell proxy (warm transform/cache) |     173.9 |   183.5 |               55.1 KiB |                  20.3 ms |
| Paired launch to index                         |     108.9 |   137.6 |                    4 B |                  50.1 ms |
| Cached short session                           |      75.5 |    86.5 |                    0 B |                  25.3 ms |
| Large local transcript tip                     |     141.6 |   150.4 |              157.6 KiB |                  85.4 ms |
| Page one older chunk                           |     127.1 |   152.7 |                    0 B |                  60.5 ms |
| 50 sustained updates                           |   1,889.8 | 1,895.5 |              10.51 MiB |                 794.9 ms |
| 25-event coalescing burst                      |      40.0 |    44.9 |              425.6 KiB |                  13.5 ms |
| Stalled-to-connected projection                |      61.5 |    78.7 |               1.09 MiB |                  33.3 ms |
| Ten repeated navigation cycles                 |   2,315.3 | 2,566.8 |               1.56 MiB |                 791.4 ms |

The initial cold browser-process shell proxy sample was 1,562.7 ms and is retained
in the artifact as `n=1`, but is not published as a trend. Native baseline results are not yet
mixed into this table; the native artifact has its own simulator and cold/warm
classes.

For the sustained-update scenario, median Chromium main-task time was 1,588.7
ms and median React commit work was 794.9 ms across 102 commits. Ten navigation
cycles reached a median sampled JavaScript-heap high-water mark of 84.0 MiB;
the median post-GC delta from scenario start was -36.2 MiB, so this run does not
establish retained growth. Raw heap samples and long-task counts remain in the
JSON rather than being turned into a flaky gate.

### Bounded-page and active-turn result

The current working tree was then measured with the same host, Chromium build,
viewport, fixture, Vite profile, cold/warm classification, and five-run command.
The only comparison is against the pre-optimization implementation state above;
both states are rooted at commit `3b3e4dc431916661e94d3d6ae5d06ad0b8998e0e`.
The first browser-process sample remains excluded, leaving `n=4` warm samples.

| Scenario                       | optimized median ms | optimized p95 ms | optimized bridge response | optimized React work | change supported by same-class evidence                        |
| ------------------------------ | ------------------: | ---------------: | ------------------------: | -------------------: | -------------------------------------------------------------- |
| Large local transcript tip     |               103.8 |            103.9 |                  11.0 KiB |              58.3 ms | bytes -93.0%; elapsed -26.7%                                   |
| Page one older chunk           |                89.3 |             90.7 |                  11.2 KiB |              41.8 ms | now a real bounded bridge page; retained DOM node still passes |
| 50 sustained updates           |             1,013.8 |          1,569.7 |                 571.1 KiB |             205.1 ms | bytes -94.7%; React -74.2%; elapsed -46.4%                     |
| 25-event coalescing burst      |                35.7 |             52.2 |                  12.2 KiB |               4.4 ms | bytes -97.1%; median one session read                          |
| Ten repeated navigation cycles |             1,467.7 |          1,479.8 |                 133.5 KiB |             481.8 ms | bytes -91.6%; elapsed -36.6%                                   |

For sustained updates, median Chromium task time fell from 1,588.7 ms to 625.2
ms (-60.6%). The optimized run still made a median 50 session reads and 100
React commits because the deliberately adversarial fixture waits for each chunk
to become visible before producing the next one. The result proves the bridge
payload is bounded; it does not claim that stream cadence or live Markdown work
is solved. No native or physical-device number is inferred from this browser
comparison.

The implemented ownership boundary is:

- the Rust bridge returns the authoritative last 40 rendered timeline items and
  a stable item-key cursor, capped at 80 even for malformed callers;
- explicit older-page reads return the preceding 40 items with gap/overlap
  tests, and the UI merges only those database-derived rows;
- active store events refresh only the selected session; health events refresh
  only fleet health; the full session index refreshes when the turn terminates;
- unchanged timeline objects retain identity and memoized expensive rows do not
  rerender when only the live assistant changes.

### Revisioned live-tail result

The next working-tree state replaced per-update session projection with a
revisioned live-tail read. It was measured with the same host, Chromium build,
viewport, fixture, profile, five-run command, and cold/warm classification. The
comparison immediately below is against the bounded-page result; the original
baseline comparison is included separately.

| Scenario             | live-tail median ms | live-tail p95 ms | live-tail bridge response | live-tail React work | change from bounded-page result                         |
| -------------------- | ------------------: | ---------------: | ------------------------: | -------------------: | ------------------------------------------------------- |
| 50 sustained updates |               548.0 |            571.1 |                  15.9 KiB |             160.7 ms | bytes -97.2%; elapsed -45.9%; React -21.7%; task -21.8% |
| 25-event burst       |                36.8 |             36.9 |                     712 B |              14.3 ms | one live-delta read; zero session/fleet snapshots       |

The 50-update fixture now performs zero full session projections. Each response
is at most 327 bytes and the complete stream is 16,319 bytes. Against the
original 11,017,769-byte result, bridge response traffic is 675x smaller
(-99.9%), elapsed time is 71.0% lower, Chromium task time is 69.2% lower, and
React work is 79.8% lower. Against the immediately prior bounded-page result,
traffic is 35.8x smaller.

The fast path is not a UI source of truth. The observer publishes an atomic
store and reconcile revision. Only response-only publications preserve the
reconcile revision. The bridge checks append continuity with UTF-8 byte length
and a checksum; the webview verifies the resulting checksum. A request change,
non-response document change, revision gap, malformed patch, terminal response,
materialization, missing live row, foreground reconcile, or observer reload
forces an authoritative paged snapshot. Terminal state is therefore never
inferred from a faster spinner or an incomplete delta.

### Structurally shared response-merge result

The live-tail measurement exposed a second O(total-store) operation upstream
of the bridge: every response-only observer patch called the general immutable
store merge. That path cloned every historical message, tool row, session, and
control-plane row and rebuilt every index even though only `AgentResponse`
could have changed.

Response-only observer patches now update the response vector and its response
indexes in place when no reader holds the prior snapshot. If a bridge reader is
concurrent, `Arc::make_mut` deliberately takes the copy-on-write path so that
reader continues to see its exact immutable revision. A mislabeled patch
containing any non-response row rejects the fast path and advances the
reconcile revision through the general authoritative merge.

The deterministic long-session fixture applies 50 response patches to a store
with 600 durable messages. All 50 take the in-place path, the message vector's
pointer and capacity remain unchanged, and the latest-response index advances
to every new response. A separate held-reader case proves copy-on-write keeps
the old response visible to that reader while the current snapshot advances.
This establishes a structural change from 50 whole-store rebuilds to zero in
the uncontended stream case. It is not published as a wall-clock or RSS trend:
the browser adapter does not execute the Rust store, and the native status lane
must run successfully before those numbers are comparable.

`desktop_observer_metrics` now reports `response_in_place_merges` and
`response_copy_on_write_merges`; the native JSONL artifact captures both. A
high copy-on-write ratio identifies a bridge reader retaining snapshots across
stream updates rather than hiding that cost as generic CPU or memory growth.

### Query-level DefraDB pagination result

`desktop_session_snapshot` no longer builds a complete rendered timeline and
then slices it. It resolves an older item key to the durable sequence space and
queries `AgentMessage` and `AgentToolCall` in descending sequence order before
projection. A 40-item page queries at most 41 message rows (40 plus one
lookahead). Tool calls have a separate 320-row structural budget; the query
fetches one sentinel row and retains every complete sequence group before the
cut. If the sentinel lands inside a group, messages at and behind that sequence
are deferred to the next page so the bridge never returns a message without its
tools. Only a single sequence group larger than 320 tool rows fails truthfully.

The durable 600-message DefraDB fixture returns 41 message rows for a tip page,
93.2% fewer than the former 600-row transcript read, and materializes 40 visible
items, 93.3% fewer than the former full projection. A tip request performs one
atomic bounded GraphQL query for both message and tool windows. An older request
first validates and resolves the cursor, then performs the same bounded query.
The cursor uses `_lt` on the durable sequence, so new tip inserts cannot shift
or duplicate an older page. Equal-sequence rows remain atomic. A page containing only non-rendering
durable rows receives a synthetic durable continuation cursor instead of
silently ending pagination, and old orphan tool groups cannot move the page
boundary. Every lookup is scoped to the selected agent and, when present, the
enrolled requester DID. Request-refresh fallback queries are also request-owned
now; they no longer reload every historical message, tool result, and compaction
row in the session.

Two independent pagination-focused review passes were run after rebasing onto
#1154. They found and drove tests for equal-sequence gaps, tool-window overflow,
requester isolation, nullable sequence handling, hidden-only continuation pages,
and orphan tool groups. A separate workstation review attempt exposed a review
harness defect: unrestricted workers can clean an uncommitted shared worktree,
and the review server readiness check can race startup. No result from that run
is treated as evidence; future workstation reviews must use an isolated clean
worktree and an observable readiness boundary.

Modern sessions retain their exact request-owned provider context accounting.
For legacy sessions without an `InferenceCall` accounting row, a tip read now
derives the fallback meter from a short-lived, selected-session DefraDB context
query. Those rows are dropped after projection and never join the global
observer. This remains a potentially expensive selected-session query, but it
is no longer permanent process residency or a second React source of truth.

### Database-owned projection-controller result

The process-wide observer no longer loads or retains `AgentMessage`,
`AgentToolCall`, `AgentToolResult`, or `CompactionEntry` content during full
bootstrap, agent-scoped reload, request refresh, or document-event handling.
Transcript events advance the authoritative reconcile revision and increment
`transcript_invalidations`; the selected screen responds with its bounded
DefraDB page. A deterministic embedded-node test seeds message, tool, and
compaction rows, proves the observer contains zero transcript-content rows, and
then proves an ephemeral selected-session query still reads the durable source
truthfully.

The React shell now has one projection controller instead of four independent
trailing queues plus separate selection, polling, and foreground effects. It
coalesces mixed work, lets a full session projection supersede a live delta,
promotes a continuity failure to one bounded database read, and refreshes the
session index once at terminal state. The 1.5-second safety poll follows the
live cursor; it no longer runs the full legacy context query on its healthy
path. Unit tests deterministically fence all five behaviors without wall-clock
assertions.

Conversation summary transcript counts are now absent when no aggregate query
was performed, rather than reporting zero from an intentionally empty observer.
React therefore holds only the bounded selected page and live overlay needed to
render. DefraDB remains the durable transcript source of truth.

Two phone-only presentation failures are fenced at the same ownership boundary.
The mobile shell tracks the visual viewport through bounded CSS variables,
rejects transient unusable WKWebView dimensions, and resamples resize, scroll,
orientation, page-show, and window-resize signals. The narrow Playwright lane
forces a one-pixel bad sample, a keyboard-sized viewport, and recovery while
asserting that the title and composer remain visible. Separately, a durable
assistant prefix now owns completed content as soon as it covers the live
overlay; an empty replacement clears that overlay immediately. The beginning
of a streamed turn can therefore no longer remain duplicated at the tail until
terminal state.

The organization follows the ownership boundaries: the former 2,027-line query
file is split into snapshot, agent-scope, document-patch, session-transcript,
and focused test modules; observer state/event handling, client-store merges and
lookups, session context/live-delta/pagination/projection, and shell projection
lifecycle are likewise separated. Application projection implementation files
target 400 lines or fewer (the largest focused observer test module is 386
lines, the main session projection is 382, and the shell hook is 377), reducing
merge hotspots for future agent work without changing provider/runtime
semantics.

### Final current-main distribution

The final current-main working tree was measured with five browser-process
samples. The first process sample is cold `n=1` evidence (355.1 ms) and is not
included below. The table is the remaining fresh-context warm distribution
(`n=4`) on the exact environment and dirty-tree fingerprint above. It is a new
baseline, not a trend against the older `3b3e4dc4` stages.

| Scenario                                  | median ms | p95 ms | median bridge response | median React work |
| ----------------------------------------- | --------: | -----: | ---------------------: | ----------------: |
| Shell proxy                               |     116.3 |  121.5 |               55.3 KiB |           13.3 ms |
| Paired index                              |      81.5 |   82.5 |                    4 B |           33.0 ms |
| Cached short session                      |      52.5 |   57.0 |                    0 B |           17.5 ms |
| Large transcript tip                      |      97.2 |   97.7 |               11.1 KiB |           52.7 ms |
| Older transcript page                     |      82.8 |   83.1 |               11.3 KiB |           37.5 ms |
| 50 sustained updates                      |     543.8 |  604.3 |               15.9 KiB |          150.1 ms |
| 25-event coalescing burst                 |      36.6 |   37.0 |                  712 B |           14.5 ms |
| Foreground truthful-connected projection  |      52.2 |   63.5 |              260.1 KiB |           22.9 ms |
| Ten repeated navigation cycles            |   1,351.9 | 1,387.1 |              158.0 KiB |          443.7 ms |

All structural assertions passed: the large tip mounted 39 rows against a
40-row limit; one prepend reached 79 against 80; the active burst used one live
delta and zero snapshots; sustained streaming used zero session snapshots;
fifty deltas totaled 16,319 bytes against 64 KiB; and the largest page response
was 11,537 bytes against 16 KiB. Wall-clock and React-work values remain
report-only.

## Initial budget policy

Hard CI failures are deterministic bounds:

- the fixture index remains exactly 120 sessions and its full desktop snapshot
  remains at most 512 KiB;
- the 600-item session snapshot remains at most 256 KiB as a regression fence
  (not a claim that sending the full transcript is desirable);
- every bridge-visible 40-item session page remains at most 16 KiB for the
  durable fixture, with a server-side maximum of 80 items;
- opening the tip mounts at most 40 transcript turn blocks;
- one older-page action mounts at most 80 total turn blocks and preserves an
  existing expensive DOM node;
- a synchronous active-turn event burst performs zero fleet/session snapshot
  reads and at most one live-delta read;
- fifty acknowledged streaming updates perform zero full session projections,
  transfer at most 64 KiB total, and keep every live-delta response at or below
  2 KiB.
- fifty uncontended response-only store patches preserve the 600-row message
  allocation and perform zero whole-store copy-on-write merges; the explicit
  held-reader case must preserve the prior revision through copy-on-write.
- a 40-item DefraDB transcript page queries at most 41 message rows and 321
  tool-call rows, materializes only complete sequence groups, and uses two
  query at the tip or two queries when resolving an older-page cursor; tool-call
  overflow marks the source as having older data, while a single group larger
  than 320 rows fails truthfully instead of being silently split.
- full/scoped observer snapshots and observer document patches retain zero
  message, tool-call, tool-result, and compaction rows; transcript database
  events publish invalidations without fetching those payloads.
- a synchronous mixed refresh wave has exactly one serialized projection owner;
  a healthy active-session safety poll requests a live delta, and only a failed
  continuity check promotes it to one bounded session query.

Elapsed time, commit work, heap/RSS growth, CPU, and long-task values are
reported only. A wall-clock limit requires at least 30 samples across multiple
days on the actual runner class with an agreed false-failure rate. Cold, warm,
simulator, device, debug, and release classes get independent budgets. A run
with a schema mismatch, offline state, stalled hydration, or terminal repair
failure is a correctness failure/state outcome, never a faster successful run.

## Evidence-backed bottlenecks and follow-ups

1. **Legacy context calculation still scans the selected transcript once.** It
   is now ephemeral and database-owned rather than globally resident, but the
   exact fallback token estimate remains O(selected-session messages) on tip,
   foreground, or structural reconcile. Add a durable database-owned aggregate
   only if it can preserve compaction/provider-view truth; do not create a UI
   cache.
2. **Historical `AgentResponse` rows remain in the observer.** The response
   stream needs a small live overlay, but completed response bodies across all
   sessions are still loaded. The next residency slice should retain only the
   active/request-index data needed for lifecycle truth and query completed
   content with the selected page.
3. **Live traffic is linear; acknowledged rendering is still per update.** The
   sustained fixture now uses zero full projections and 15.9 KiB total, but it
   deliberately waits for every update to become visible and therefore records
   100 profiler entries. Measure real provider cadence before adding a
   frame-window reducer; a terminal update must always flush and revision gaps
   must always reconcile.
4. **The entire 120-row session index renders.** The desktop snapshot is about
   55 KiB and the DOM has 120 conversation rows. Follow-up: DB-backed cursor
   pagination plus list virtualization, after preserving the eager mobile index
   contract from #1141.
5. **Foreground projection is the largest remaining browser bridge payload.**
   The deterministic truthful-connected projection transfers 260.1 KiB. Split
   repair/health evidence from unrelated control-plane projection only after
   the sync owner defines the authoritative healthy/stalled/terminal boundary;
   do not substitute UI-local status.
6. **Suspend/resume performance is unknown, not fast.** Add real foreground and
   network-change boundaries with #1143, report truthful stalled/terminal state
   with #1144, and validate the product decision in #893 on a device.

The harness itself found one independently bounded rendering defect before the
final baseline: one page request recursively triggered the scroll threshold and
mounted 199 turn blocks. The same host/browser/fixture after removing recursive
scroll paging mounts 79 and retains the prior node. This is a structural
before/after result; timing improvement is not claimed from the single pre-fix
sample.

Proposed follow-up issue slices, in dependency order:

1. Add the documented remote hydration fixture on the landed #1154 foundation
   without duplicating its lifecycle; accepted hydration merges must advance
   the authoritative reconcile revision.
2. Replace the ephemeral legacy context scan with a truthful database-owned
   aggregate, then bound observer `AgentResponse` residency to active requests;
   acceptance: bootstrap memory is independent of historical transcript and
   completed-response content bytes.
3. Measure a terminal-flushing frame cadence reducer on real provider streams;
   acceptance: fewer commits without losing any acknowledged terminal or
   reconcile state.
4. Add cursor-backed session-index virtualization; acceptance: 1,000 durable
   index rows keep bridge and mounted-row counts bounded without a second cache.
5. Stabilize signed route publication across authenticated enrollment before the
   receipt is accepted; acceptance: the corrected native lane reaches paired
   index repeatedly without relaxing transport-DID, generation, or route checks.
6. Extend `mobile-hydration-v1` after #1142/#1143 and expose truthful progress
   after #1144.
7. Put the native build/smoke artifact on the macOS runner under #890, then
   establish simulator wall-clock distributions.
8. Fail measurement classification loudly on pair-time schema skew and surface
   merge rejection with #1122; schema-incompatible runs are invalid performance
   samples.

## Conclusion

The measured baseline has two same-environment UI/bridge optimization stages.
Bounded pages first removed more than 90% of the large payload classes. The
revisioned live-tail path then removed 97.2% of the remaining sustained bridge
traffic and all 50 intermediate full session projections. End to end from the
original stream baseline, bridge traffic is 675x smaller, elapsed time is 71.0%
lower, task time is 69.2% lower, and React work is 79.8% lower. The third,
desktop-core stage removes all 50 uncontended whole-store rebuilds structurally.
The final query stage reduces the durable 600-message page read to 41 message
rows and the visible projection to at most 40 sequence-atomic items. Tool reads
use 321-row overscan, defer incomplete groups instead of failing long sessions,
and retain a cursor even when a durable window renders no visible item. The
database-owned extension then removes all historical transcript-content rows
from the global observer and replaces competing React refresh owners with one
bounded controller. Native wall-clock and memory impact remain intentionally
unclaimed until the native lane produces a valid distribution. Native cold
launch, ephemeral legacy context-query cost, completed-response residency,
suspend repair, device memory/energy, and remote hydration remain unknown until
their named lanes or dependencies exist. The next smallest major optimization
slice is to remove completed `AgentResponse` bodies from global observer
residency and replace the legacy selected-session context scan with a truthful
database-owned aggregate; together those make bootstrap memory independent of
historical transcript size without creating a second cache.
