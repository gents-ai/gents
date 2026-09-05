# Stock Grok UI hydration completion audit

The unchanged stock Grok TUI is the compatibility target. All work stays in
PR #1363; #1324 is superseded. Green CI for the background fixes is not proof
that the broader hydration goal is complete. Never merge automatically.

## Verified background baseline

Commit a8a9d5bd passed all 11 CI jobs, 307 local shim regressions, and the full
workspace check. The runtime suite passed 2,705 tests (3 ignored). Live GLM
and unchanged Grok 1.0.13 checks cover child task output, native bash kill,
native subagent stop/cascade, later-turn steering, and automatic wakeups.
See grok-background-projection.md and PR verification comments for evidence.

## Remaining requirements and evidence needed

| Surface | Current evidence | Completion evidence required |
| --- | --- | --- |
| Session resume | Stock dashboard replay/continuation passed; expanded cancelled child bash and its result now render | Maintain regression coverage for read-only replay and active handoff; final review pending |
| History picker/search | Authorized search/pagination and stock F3 listing passed; native dashboard attach passed | CLI/F3 selection still has a stock local-file preflight for database-only sessions; use the native leader dashboard |
| Context usage | Live capture-comparison probe and rendered counter/breakdowns passed; stale/decreasing context regressions pass | Final review pending; unsupported provider breakdowns remain explicitly partial |
| Cumulative token accounting | Physical parent/child aggregation, retry deduplication, duration, native billing absence, and rendered usage verified | Reasoning/cache-creation/per-model pricing are not persisted and cannot be recovered as exact breakdowns; final review pending |
| Other native controls/data | Broader inventory incomplete; interjection/manual compaction are shaped stubs, model/mode capabilities need review | Inventory the stock client's requested session/info, usage, compaction, goal/todo, model/mode, replay and task surfaces; implement runtime-supported hydration/control paths and explicitly document genuine unsupported capabilities |
| Final gate | Current local runtime: 2,711 passed, 3 ignored; shim: 317 passed; workspace passed; stock live checks passed | Independent review and green CI on the final published tip remain required |

## Ownership and implementation constraints

- Resume replays persisted observations; it must not regenerate a conversation,
  create replacement requests, or invent runtime completion transitions.
- Reuse the existing projection leaves and send-success cursors. Fence the
  replay/live handoff so active requests and late background events are not
  duplicated, missed, or terminalized ahead of their canonical owners.
- Apply exact session/principal/requester authorization before exposing any
  history, child edges, usage, or controls. The existing runtime history helper
  is principal-scoped, so it is not by itself a requester-scoped UI boundary.
- AgentSession has no persisted cwd/model/usage fields. Do not report the
  current client directory as historical execution evidence or invent session
  settings that do not affect the runtime.
- InferenceCall has no session_id. Resolve physically owned requests first;
  use its `context_accounting_json` and provider usage through the existing
  accounting owner. Avoid an unrelated agent-wide latest-call estimate.
- ContextAccounting is captured from the exact RenderedRequest before
  provider dispatch. The runtime `context_budget` and `session_history`
  modules already parse/account these records; extend/reuse those seams.
- Changes to legal runtime transitions or provider input start in Lean, then
  conformance tests, then implementation. Read-only UI replay is not a new
  model-execution lifecycle.

## Source map

- Gents: `grok_shim/acp.rs`, `projection.rs`, `turn.rs`;
  `gents/src/toolset/session_history.rs`, `context_budget.rs`;
  protocol `InferenceCall` schema and runtime AgentSession schema.
- Stock client: `app/effects/mod.rs` (LoadSession, FetchSessionList),
  `app/effects/helpers.rs` (picker DTO parsing), `app/session_startup.rs`,
  `app/acp_handler/mod.rs` (context usage and replay/live delivery).

The next implementation slice is persisted session observation/accounting
with requester scoping, followed by list/load replay and real stock-client
resume verification. The complete objective remains all rows above, not just
the already verified background baseline.

## Accounting implementation progress

`toolset::load_session_inference_observation` now supplies exact
session/principal/requester-scoped usage and latest context. It joins inference
calls by physical request document identity, batches request predicates, and
reuses the existing usage aggregation. Compaction calls contribute to usage
but cannot replace the conversation's inference context. Missing requester
means exact null identity, not unrestricted access.

All 9 session-history tests passed, including new foreign-requester/physical
ownership and context-decrease-after-compaction regressions. The bounded
request observation API is now wired into live metadata, ordered by persisted
dispatch timestamp/sequence/identity. A newer inference can lower context;
polling an older request cannot replace it. Obsolete per-request cumulative
token cursor plumbing has been removed.

All 308 shim regressions passed before that final plumbing cleanup. The live
`grok_context_probe.py` passed on session `grok-edge-730ed1622a554e1a`:
calls `9bff3176-734d-401f-91e5-5fd18f8009e8` and
`64760abb-ce79-4eee-b95e-8303b317e7f9` projected 7,854 and 7,891 context
tokens, respectively, exactly matching persisted accounting plus completion
tokens. Each turn generated only 6 tokens, demonstrating the distinction
from cumulative generated-token spend. This is wire verification, not yet a
resume check. A subsequent unchanged Grok 1.0.13 fullscreen test displayed
`STOCK_CONTEXT_OK` and the native `7.86k` counter in session
`123b5940-5e14-4adf-918b-84f5b132d741`. Its exit banner offered `--resume`,
which reinforces why session/load must be implemented rather than left as a
shaped stub. Full package/workspace gates and the remaining requirements
above are still open.

The resume prerequisite audit also found root session discovery scoped only
by session ID. It now requires the bound principal as both agent and requester
(matching root submission), with a regression covering foreign agent,
foreign requester, and missing requester rows. Child session discovery keeps
its separately authorized child identity. This is a read-only scope fix, not
a new runtime lifecycle.

## Session history and resume progress

- `sessions.rs` reads exact principal/requester-scoped request pages, checks
  persisted session owners/behaviors in batches, and derives history from
  those rows. It provides search, activity ordering, and keyset page cursors;
  no current cwd/model is invented as historical metadata. Child sessions
  remain under their parent. New AgentSession rows now persist requester_did;
  older null-requester sessions require actual authorized request history.
- `session/load` uses the existing projection and its send-success cursors,
  marking historical deliveries with isReplay. Live observation starts after
  the load response, from the attachment time captured before history reads.
  Runtime execution and session lifecycle are not restarted or rewritten.
- 312 shim tests passed before the subsequent active-handoff test and roster
  additions. The 70-message replay regression covers multiple DB pages,
  one prompt echo, and no new requests. All 9 scoped accounting tests pass.
- After restarting the server, `grok_resume_probe.py` loaded persisted session
  `123b5940-5e14-4adf-918b-84f5b132d741`: request count remained 1 during
  load, and exactly one new live continuation recalled `STOCK_CONTEXT_OK`.
- Actual stock 1.0.13 F3 lists/searches server history. Its selection path,
  like CLI --resume, requires local Grok session files before ACP; selecting
  a database-only session reports "Session not found locally". This is not
  a successful stock resume check. No client files have been fabricated and
  no client source has been changed to bypass it.
- The stock leader dashboard has a distinct roster attach path that directly
  issues session/load. `x.ai/sessions/list` now projects that native shape;
  actual rendered attach verification is next. This is the intended native
  server-hosted path, not relabeling coding sessions as cloud chat entries.
- The active-handoff regression found that fractional attachment timestamps
  exclude subsequently created requests within the same second because
  request submission persists whole seconds. Discovery now includes the
  full attachment second; existing cursors deduplicate already replayed rows.
  All 313 shim tests passed, including that active-handoff regression
  (`/tmp/grok-roster-tests-fixed.log`).

### Stock dashboard evidence and usage followup

The unchanged Grok 1.0.13 dashboard (`Ctrl+\\`, expand Inactive, select a
roster row) attached to `123b5940-5e14-4adf-918b-84f5b132d741` and displayed
replayed content. A subsequent live prompt returned `DASHBOARD_RESUME_OK`
in that same session. Native history fidelity still needs the tool/child
resume cases and a repeat rendered check after preserving original human
prompt IDs: the stock client hides the `notifications-*` family, so assigning
that family to every historical request hid human prompt text. The fix
preserves persisted promptId for replay while keeping the existing live
observation/cancellation identity for a resumed active request. All 313 shim
tests passed after that change (`/tmp/grok-resume-origin-tests.log`).

`x.ai/session/usage` is now wired to the shared runtime accounting aggregation,
plus physically authorized descendant traversal. Each exact child session
scope is counted once even if multiple edges reach it. Open/missing/unreadable
usage is marked incomplete. Costs are absent and marked partial, never
assumed free; unavailable model/duration/reasoning/cache-creation breakdowns
are identified in metadata. Distinct main-loop rounds exclude compaction and
retry attempts using persisted dispatch coordinates. This implementation
now passes embedded physical-lineage coverage: parent and child usage are
included despite aliased logical request IDs, while a foreign requester's
usage is excluded. All 314 shim tests passed in
`/tmp/grok-info-usage-regressions.log`. Live stock-panel verification remains
outstanding.

`x.ai/session/info` now projects the native nested response envelope, current
context accounting categories, main-loop turn count, and physically scoped
compaction counts. Breakdown fields absent from runtime accounting remain
explicitly partial rather than invented. The whole-workspace check passed
in `/tmp/grok-hydration-workspace-check.log`; subsequent accounting hardening
still requires a fresh gate. Turn counting now uses physical request identity
and marks unknown call kinds/ownership unavailable. A new regression covers
retry deduplication, separate physical requests sharing a logical alias, and
compaction exclusion (`/tmp/grok-accounting-physical-rounds.log`, pending).

Remaining: stock dashboard resume including tool/child histories, the local
history-picker limitation assessment, cumulative usage/session-info and the
broader native-control inventory, and full runtime/workspace/final-PR CI gates.

### Latest live panel evidence and remaining fidelity gaps

- Full runtime gate completed: 2,707 passed, 3 ignored, no failures
  (`/tmp/grok-hydration-full-runtime.log`). The physical-round accounting
  slice subsequently passed all 10 tests; it still needs rerunning after
  explicitly excluding the runtime's `oneoff` calls alongside compaction.
- Extended live context probe passed on `grok-edge-13d16b43358448fe`:
  context 7,854 then 7,891; cumulative input 14,298, output 18, total 14,316,
  three billable calls including one `oneoff` call, two inference rounds.
  Every wire total was compared against physically owned database rows.
- Stock 1.0.13 dashboard replay now visibly includes both human prompts and
  answers. `/session-info` renders the model/backend, turn index, and context;
  its Context usage and Usage limit tabs display the same persisted data.
  Direct `/usage` opened a client subscription question; the session-info
  modal's tabs expose the actual accounting without client modifications.
- Rendered fidelity is NOT complete: stock ignores our custom partial-field
  metadata and displays absent system-token/tool-count/reasoning/duration
  fields as zero. The exact `RenderedRequest.request_json` capture exists and
  should supply supported context details through a runtime observation
  owner. Existing inference telemetry should supply available durations.
  Do not treat passing wire totals as proof these breakdowns are correct.
- The usage tab also tries `x.ai/billing`, currently method-not-found. Audit
  the native local-provider billing shape rather than inventing cloud credits.
- Stock dashboard roster polling is already built into its event loop while
  the dashboard is open; no additional push-state owner is necessary.

### Duration and local billing implementation

The next accounting owner extension aggregates `InferenceCall.started_at` to
`ended_at` durations, excluding queue time. Missing, reversed, invalid, or
overflowing intervals remain unknown rather than known zero. The shim sums
available durations across its existing authorized scopes and marks partial
duration explicitly. Tests cover closed intervals, missing/reversed timestamps,
empty histories, and the native duration field. These additions are not yet
live-verified; `/tmp/grok-billing-duration-shim-tests.log` is the new gate.

`x.ai/billing` now returns the native nullable config envelope with no cloud
balance or subscription and on-demand billing disabled. This matches the
stock modal's "No billing data available" path; it does not fabricate a
credit allowance or claim local inference is free. A route regression is
included. Rendered verification after rebuilding remains required.

The preceding shim suite completed with 315 passing tests. Its result does
not cover these newest duration/billing changes or the still-missing detailed
context hydration. The shared live probe now expects loadSession=true.

### Captured context details (implementation, verification pending)

`toolset::load_session_context_details` now resolves the exact authorized
physical request and its inference turn/attempt capture, then checks the
provenance admission call ID. Only numeric details leave this helper. It
derives OpenAI chat system/developer estimates and actual message/tool counts,
using the same runtime JSON byte estimator and preserving the total message
partition. Unsupported provider shapes, document-injected partitions, missing
captures, and ambiguous ownership remain unavailable rather than guessed.
The shim wires those fields into session-info. A pure decomposition test and
extended live capture-comparison probe were added; embedded authorization
coverage and actual rendered verification remain outstanding.

Current checks: `/tmp/grok-context-details-check.log` and
`/tmp/grok-context-details-runtime-tests.log`. The preceding accounting slice
passed 10 tests; that result predates the new capture helper and duration test.

The capture-owner regression has now been added to the embedded session
history fixture. It verifies foreign principal/requester/session rejection,
admission-call mismatch rejection, and physical ownership despite a misleading
capture request alias. Current runs are `/tmp/grok-context-ownership-tests.log`
and `/tmp/grok-context-decomposition-tests.log`; results remain pending.
The new dev build is `/tmp/grok-context-details-live-build.log`.

Latest remote check: fetching main still leaves zero main commits missing
from this branch. PR #1363 remains clean and its existing a8a9d5bd tip has all
11 checks passing. That is baseline evidence only; none of the current dirty
hydration changes have been pushed or covered by final-tip CI yet.

The full session-history slice now passes 13 tests, including captured-context
ownership/admission rejection and numeric decomposition
(`/tmp/grok-context-ownership-tests.log`). The dev binary is rebuilding with
the capture helper. Fresh broad gates are queued for this source state:
`/tmp/grok-hydration-final-workspace.log`,
`/tmp/grok-hydration-final-runtime.log`, and
`/tmp/grok-hydration-final-shim.log`. These are pending, not green evidence.

### Updated binary: live and rendered verification

The expanded probe passed on `grok-edge-7f5059b3e1804437`
(`/tmp/grok-context-details-live.log`): context 7,854 then 7,891; final usage
14,296 input + 19 output = 14,315 total; three calls, two inference rounds,
1,635 ms provider duration. Counts matched captured provider bodies, usage
matched physical inference rows, duration matched timestamps, and billing
returned its native no-data shape. The final workspace check also passed.

Unchanged Grok 1.0.13 dashboard resumed that session. Its context panel now
shows 480 system tokens and 21 tool definitions; session usage shows 14,315
tokens and 1.6s API time. The billing panel displays "No billing data
available", without the prior unsupported-method error.

The same client also resumed old child session parent
`0d5413ba-3ebc-490c-b3cd-366d8a7aa3cf`: original human prompt, subagent call,
parent answer, and later interruption acknowledgement rendered from history.
Detailed expanded child-output inspection remains pending; do not infer it
from the parent acknowledgement. Final runtime/shim gates are still running.

Expanded child inspection found a wire defect: the cancelled bash call was
persisted and emitted on the correct child session, but its tool `content`
used bare `{type:"text",text:...}` instead of ACP ToolCallContent's
`{type:"content",content:{type:"text",text:...}}` envelope. The unchanged
client displayed the child's reasoning but not that tool. The shared result
projection is now corrected, with golden coverage and the live tool-update
expectation updated. Retest the rendered child before claiming the cause
fully resolved. Builds/tests: `/tmp/grok-tool-content-build.log` and
`/tmp/grok-tool-content-shim-tests.log`. The preceding shim gate passed 316
tests but did not validate this envelope with the stock decoder.
Reference: https://github.com/agentclientprotocol/agent-client-protocol/blob/main/schema/v1/schema.json

The corrected binary was rechecked in unchanged Grok 1.0.13: expanding the
same resumed child now shows `Run sleep 180`, and opening that tool displays
`tool call cancelled`. This confirms the missing child tool was the content
envelope defect, not missing database state or a client modification.
The workspace gate passed again (`/tmp/grok-tool-content-workspace.log`).
The full runtime gate completed with 2,711 passed and 3 ignored. The shim
rerun found one old test reading the former `/text` path; it now asserts the
correct `/content/text` path and is rerunning in
`/tmp/grok-tool-content-final-tests.log` (not yet passed).
