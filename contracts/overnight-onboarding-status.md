# Overnight onboarding acceptance — 2026-09-14

## Scope and delivery

Active user goal: make fresh-home onboarding/configuration/chat reliable through
repeatable live acceptance, not just happy-path UI tests. Preserve existing homes,
the current user demo, unrelated runtimes, and the parked Defra dependency bump.
The user subsequently authorized a **reviewable `gh stack` of PRs**. Do not merge.

Working tree: `gents-onboarding-demo-regressions`, starting branch
`feat/compact-local-onboarding`, starting HEAD `b3015d6b0`. The large existing
uncommitted onboarding/inference changes are part of the user's work and must be
preserved and included in a coherent baseline PR. New scheduling/proof changes
and configuration controls should be separately reviewable stack layers.

## Observed first-chat stall

- Fresh demo home: `/tmp/gents-demo-fresh-6NpGk0`; desktop process 22305.
- Agent Forge, GraphQL `http://127.0.0.1:9291/api/v0/graphql`.
- Request `cd17b543-a572-49f6-8a9d-382c9ff57276`, session
  `699dc7b0-6c24-4c0b-9d27-8cec2a01cdc2`.
- Native log: home `native.log`; runtime tracing: `desktop/logs/desktop.log`.
- Read-only macOS process sample: `/tmp/gents-stall-22305.sample`.
- Last successful response-progress persistence: 21:30:02.497 UTC.
- Canonical write gate owner: `admission.persist_existing_call_terminal`.
- Heartbeat, hydration-served, runtime-status, backend observations, and response
  progress all timed out waiting behind this owner.
- At 21:34:03.313, the fourth response-progress attempt timed out, and dropping
  the stream cancelled its finalization at **240714 ms**. Gate then recovered.
- Inference metrics recorded provider completion, while the durable response
  remained streaming with the same truncated content shown by the desktop.

### Reproduction and cause

`hold_stream_guard` awaited terminal-call persistence inside the stream's
`next()` future. The daemon selected a response-flush timer and awaited a write
through the same canonical gate, without polling that stream again. The gate
owner and its timeout were therefore suspended inside the losing select branch.
This is a Gents future-polling dependency cycle, not evidence that this particular
Defra primitive was non-yielding. Earlier descriptions of the incident as an
upstream primitive stall must not be used as an established diagnosis.

Deterministic red test:
`admission::stream_guard::tests::terminal_persistence_progresses_while_consumer_waits_for_shared_write_gate`.
Before the fix it fails in 250 ms with "finalization stranded its write gate when
consumer stopped polling" (`/tmp/gents-overnight-stall-red.log`).

Implemented fix: independently schedule finalization, retain an abort-on-drop task
handle inside the stream, and still await durable finalization before exposing
the terminal provider item. Existing permit Drop repair and terminal-winner
guards retain ownership. This does not promise preemption of arbitrary storage.

## Acceptance status

- [x] Focused green stream ownership and cancellation regressions: nine stream
  tests plus the real AdmissionPermit abort/terminal-repair regression.
- [x] Lean model/conformance for polling availability vs elapsed timeout:
  full `lake build` and byte-for-byte generated scheduling fixture check.
- [x] Actual ConfigAccess/Defra adapter contention regression.
- [x] Full runtime package tests, including final network-narrowing changes:
  2,855 passed, zero failures, five expected ignores.
- [x] Final workspace/all-target check before push.
- [x] UI field audit, guided selection, validation and save/reload tests:
  full desktop unit suite green; remaining JSON-only fields and dedicated reload gaps are
  explicit in `tools-context-ui-coverage.md`.
- [x] Live native paired-fixture chat, configuration activation and interruption
  with follow-up requests. One unreproduced interruption delay remains tracked.
- [ ] Interactive managed onboarding/restart acceptance: macOS is locked.
- [x] Configurator-created behavior, native graph install/run/result, and native
  background process/result acceptance in a temporary root. Explicit network
  isolation exposed a missing model-facing control; the control and a live
  sandboxed command/loopback-denial check are now verified.
- [x] Native build and fresh home prepared; visual native acceptance needs unlock.
- [x] Reviewable draft PR stack with evidence, boundaries, and remaining manual checks.

Account sign-in requires user consent; no automated OAuth acceptance claim.
Multiple simultaneous accounts per provider remains separate from this scope.

## Integrated automated checks already run

- Desktop production build: passed.
- Desktop unit suite: 379 passed, 13 intentionally skipped.
- Runtime package: 2,855 passed, five expected ignored.
- Workspace/all-target check: passed.
- Desktop core: 236 passed; bridge: 157 passed, two ignored.
- Login packages: 15 tests passed.
- Final network writer persistence/omission/widening, effective inspection,
  conformance selection and CLI Setup prompt regressions: four focused tests
  passed. Review then added explicit network-input laws to the existing persona
  command model, all 12 mode/input conformance combinations, and an owner-local
  enforcement-disclosure test. The underlying execution policy is unchanged.
- Lean: 1,090 targets built successfully.
- Desktop client suite: 31 passed.
- First-run Playwright: 27 passed across three viewport projects. These use the
  browser harness and do not prove native provider account consent or pairing.
- Native desktop and bridge runner build: passed, with the existing macOS large
  unwind-section linker warning.
- Live driver repairs: 25 focused infrastructure tests and TypeScript passed.
  The focused behavior test now edits the canonical Context, reloads it, and
  checks a subsequent real response; the broad legacy config-flow driver still
  requires migration and must not be reported as accepted.

## Stack and review runtime

Published as a draft `gh stack` (GitHub stack #1492); not merged:

1. `feat/compact-local-onboarding`: compact setup and shared provider defaults,
   account/callback presentation, profile binding, and Claude input conformance.
2. `fix/stream-finalization-polling`: independently polled terminal persistence,
   Lean scheduling model, generated conformance, and real gate/permit tests.
3. `feat/onboarding-tool-controls`: canonical tool/context controls, validation,
   coverage matrix, and current live context/chat navigation acceptance.
4. `fix/onboarding-provider-review`: review findings, canonical Claude capability
   consumption, account/discovery races, and live fixture observation plumbing.
5. `fix/configurator-network-policy`: expose existing disabled network policy
   through the protected sibling tool writer; preserve omitted settings.

PRs in order: [#1487](https://github.com/gents-ai/gents/pull/1487),
[#1488](https://github.com/gents-ai/gents/pull/1488),
[#1489](https://github.com/gents-ai/gents/pull/1489),
[#1490](https://github.com/gents-ai/gents/pull/1490),
[#1491](https://github.com/gents-ai/gents/pull/1491).

An ignored worktree-local `.gents` runtime listens on loopback port 9491. The
installed `code_review` graph successfully reviewed `origin/main..0c6e99cb9` using
`GLM-5.3-Flash-NVFP4` at `http://workstation-1:8000/v1`; its stage sampling is
temperature 1/top-p 0.95. This instance is separate from the preserved user demo.

## Live runtime evidence

Isolated home `/tmp/gents-overnight-acceptance-JBebJk/agent`, temporary project
alongside it, loopback GraphQL port 9493. GLM sampling: temperature 1/top-p 0.95.
These CLI/native-tool checks do not substitute for desktop P2P acceptance.

- Setup request `99d6589d-de73-47f5-8ec2-9904d098b52e` created Fixture Coder,
  changed the principal default, and retained Setup as its original configurator.
  Canonical behavior/context/tool documents were independently inspected.
- Request `525b2e9e-ac2f-4518-a6c4-bb8723032990` selected the default coder,
  reproduced a deliberately broken addition test, fixed it, and ran it green.
  An independent `node --test` also passed.
- Setup request `544a64ea-acc7-43b8-a44f-796d07108096` used native `install_pack`
  for code-review. No model-built CLI or external runtime was needed.
- Coder launched graph `7b5e7a68-813e-4576-bd60-87733e01d496` through native
  graph tools and returned its handle. A subsequent request in the same session
  read the durable successful result: all four stages completed, no findings.
- Request `46a64b2f-309b-47ed-accc-618fc8531290` launched a 15-second background
  process and returned without waiting. Follow-up
  `a9c30452-49c3-4603-85b7-71790c8cc818` used native process tools to confirm
  completed/exit code 0 and drained output.
- A second 120-second process accepted a follow-up while still running and was
  cancelled after about 18 seconds. Request
  `0cba4f27-b71f-4541-84d4-baab977a5c3e` confirmed terminal cancellation using
  native cancel/list/read tools. Independent CLI background inspection confirmed
  the durable states, rather than relying only on the model's narrative.
- Restarted the isolated runtime on the rebuilt binary without replacing its
  home. Setup request `a37eb6aa-50c0-4d64-8661-4544c1789aed` persisted and
  inspected disabled network policy on the existing coder. Coder request
  `28e19c44-da47-42db-a187-881fb0a83d41` passed `node --test` under the sandbox
  and failed an authorized loopback curl probe immediately. Outside that sandbox
  the same listening port returned HTTP 404, confirming the listener was present.
  This tests host-command enforcement, not inference/remote-tool network policy.
- The live desktop fixture exposed two stale harness assumptions: it did not
  re-establish ephemeral readiness after startup deliberately cleared it, and its operator
  config routes addressed the desktop node despite a deliberately dummy GraphQL
  endpoint. Repairs establish readiness once after verifying both manual route
  legs, wait on observed peer readiness, and invoke canonical config
  commands on the actual fixture runtime node. Requests remain client-local.
  Production restart readiness invalidation is unchanged.
- Three-turn chat passed in one session. Context save/reload plus a real model
  marker response passed four fresh runs, including three repetitions. The test
  waits for the existing active/router generation and idle reconciliation;
  durable Save alone is not runtime activation (the watcher debounces for five
  seconds). No fixed sleep or relaxed model-response assertion was substituted.
- Interrupt plus same-session follow-up passed four consecutive runs. An earlier
  >30-second disabled composer was not reproduced or root-caused. It is tracked
  in [#1493](https://github.com/gents-ai/gents/issues/1493), with failure-only
  request/terminal diagnostics retained in the test. Do not claim it is fixed.

## Review disposition

GLM review run `25b6a0aa-5bd0-4443-90bb-96bf739b5014` completed successfully.
Addressed callback provider labeling, duplicated Claude capability ownership,
disabled-account presentation, null timeout preservation, visible invalid-limit
validation, Codex normal-context versus maximum-context distinction, and
duplicate provider/auth mapping. Duplicate findings were folded together.
The proposed removal of the Context-to-Tools picker was rejected: it selects
the existing canonical Tools document rather than introducing another owner.

A second GLM review, `371792b1-babc-41ca-b6aa-19d9b8aa818d`, completed against
the network writer. Its accepted findings are addressed: the optional narrowing
input now has laws and conformance in the existing PersonaRequest model, and
the enforcement description lives with CommandExecutionPolicy and is tested
against its real validator. Other proposed parser/prompt duplications were
refuted as established ownership patterns, not new abstractions.

Initial CI found two packaging/formatting omissions: the new shared login UI
crate was missing from the support shard, and two frontend files needed
Prettier. Fixes are assigned to the introducing stack layers; subsequent CI must
be inspected rather than treating local checks as a green GitHub result.

## Next hands-on acceptance checklist

Use a fresh desktop and managed-agent home; keep earlier demo homes intact.
Mark actual results, not the presence of controls, as acceptance.
Fresh root: `/tmp/gents-morning-onboarding-ZX41uV`; desktop process launched via
Tauri dev against Vite on `http://127.0.0.1:1426`. macOS reports
`CGSSessionScreenIsLocked=Yes`, so visual native acceptance and bringing the
window forward require the user to unlock. No attempt was made to unlock it.
The managed agent home remains uninitialized for the user's onboarding run.

- Local setup: default home root, folder selection, each authority preset,
  editable name, launch errors, and direct transition into inference.
- Remote setup: connection details, failed connection/retry, and real readiness.
- Each account provider: one browser launch, correct callback branding, visible
  account identity, credential expiry, discovery, model and reasoning choice.
  OpenAI/ChatGPT, Claude subscription, Grok, and OpenRouter require actual user
  credentials/consent; local GLM can be tested automatically.
- Saved inference: selected backend/profile/model/sampling survive navigation
  and restart; no placeholder model/profile takes over; add-backend uses the
  same flow and does not replace a different provider's credential.
- Chat: complete first response, follow-up, double-Enter, stop, navigate away
  during streaming, reopen session, resume bottom-follow, and restart/reopen.
- Configurator: create a separate coding behavior, persist its literal prompt,
  set it default, preserve Setup, inspect real permissions, and run a test task.
- Graph/background: install via native pack tool; run via native graph tool;
  obtain a handle and send another message; inspect result and cancel safely.
- Contexts/Tools: linked document pickers, subagent target selection, Remote
  Tools exact-name grants, invalid input feedback, Save/Cancel/reload, and no
  permissions silently enabled by discovery or defaults.
- Failure states: explicit useful error, retry or recovery path, and no indefinite
  busy composer after the canonical request becomes terminal.

Do not infer physical iOS/Android acceptance from desktop or browser tests.
