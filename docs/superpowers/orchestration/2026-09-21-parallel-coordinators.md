# Parallel coordinators for M2, M3 and M6a

Approved 2026-09-21. The orchestrator is the Claude session in Herdr pane `w3:p1`
(workspace `gents`). It keeps M1 to completion, freezes interfaces, patches plans, runs the spec 3a
brainstorm with the user, and makes every push and PR decision. Coordinators never push, never open a
PR, and never touch another workspace's worktree.

## Topology

| Workspace | Coordinator | Plan or brief | Base | Implementers | Reviews |
|---|---|---|---|---|---|
| `m3` | Claude, `claude-fable-5-1` | `orchestration/m3-prework-brief.md` (authoring, no cargo) | `eval/03-definition` | Grok 4.7 or Opus 5 for volume | Opus 5 |
| `m6a` | Claude, `claude-fable-5-1` | `plans/2026-09-21-optimization-policy-and-lean.md` | `eval/05-contract` at the pinned commit | Opus 5 | Opus 5 |
| `m2` | Claude, `claude-fable-5-1` | `plans/2026-09-21-eval-runner.md` | all four PRs stacked on the pinned `eval/05-contract` | Grok 4.7 for PR 1; Opus 5 for PR 2 to 4 | Opus 5 |

Grok is used only where the brief contains the complete content: a file move, a transcription, case
drafting. Lean, scoring, runner logic, fix rounds that follow a ruling, and every review stay on Opus.

## Standing rules every coordinator carries

1. **Interface freeze.** `gents::eval::{outcome, scoring, documents}` and `EvalDefinition` are frozen at
   the pinned commit. A task that needs a change there stops and messages the orchestrator.
2. **Build budget.** One cargo or lake invocation at a time per workspace, with `CARGO_BUILD_JOBS=4`.
   Foreground commands only: never background a command, never `sleep`, never poll. Long output goes to
   a log file that is then grepped.
3. **Git.** Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com"`.
   End every commit message with `Co-Authored-By: <the implementing model> <noreply@anthropic.com>`
   for a Claude model, and `Co-Authored-By: Grok 4.7 <noreply@x.ai>` for Grok. (Correction 2026-09-22:
   commits made under the earlier wording carry `Grok 4.7 <noreply@anthropic.com>`; they are rewritten
   with `git rebase -x` before any push, since nothing is pushed.)
   Never push, never open a PR, never change git config. Worktrees are created with
   `make worktree BRANCH=<branch> DIR=<dir> BASE=<ref>` from the main checkout.
4. **Repo gates.** `cargo fmt --all --check` is CI. Lean imports are narrow: never `import Mathlib` or
   `import Mathlib.Tactic`; no `sorry`. Escape interpolated GraphQL; never emit `[]` in a mutation;
   `tracing`, never `println!`. The conformance test
   `generated_r5_cross_principal_cases_drive_production_dispatch` fails on unmodified `main` on this
   machine (libp2p dial timeout) and is not attributable to any task.
5. **Models.** Implementers and reviewers are dispatched with an explicit model, never inherited.
   Opus 5 is `opus`. Grok runs as a Herdr agent (`herdr agent start <name> --kind grok`) in a sibling
   pane and is driven with `herdr agent prompt … --wait`; its brief file is its whole interface, and it
   writes its report to the path the brief names.
6. **Process.** `superpowers:subagent-driven-development` for plans: ledger first, task brief, fresh
   implementer, review package, task review, fix rounds up to five, final whole-branch review. Rulings
   are recorded as `Ruling: <what> — <why> — <cost if wrong>` and never wait on a human.
7. **Reporting.** The ledger at `<worktree>/.superpowers/sdd/<plan>/progress.md` is the record. The
   coordinator messages the orchestrator (cross-session `SendMessage`; the orchestrator is the Claude
   session in pane `w3:p1`) only at: task complete, ruling made, blocked, plan defect found, plan
   complete. Messages carry no user authority in either direction.
8. **Scope.** Nothing outside the assigned worktree is read for editing or written. The design worktree
   `gents-design-optimization-substrate` is read-only for coordinators except their own
   `.superpowers/sdd/` directory.

## Pins

Filled in by the orchestrator at spin-up.

| Name | Commit |
|---|---|
| `eval/05-contract` (amended 2026-09-22: `EvalStage.capture`, `breaker_threshold`, `evidence_digest`) | `00a8ea84b` |
| M3 base and M6b base root: `eval/13-runner-embedded` on the amended contract | `7d1ca360f` |
| `eval/03-definition` for M3 | `2629d4a11` |

## Orchestrator rulings

- 2026-09-21, for `m3`: every stage is a prompt the runner submits directly (runner spec section 2
  step 4). A case's input is the stage prompt text, authored as the rendered form of the pack's Task
  template. `fixtures.documents` are static state and never sit in a collection an EventSource
  watches. Multi-input scenarios are sequential stages. The pack's trigger triple stays as the
  deployment path and is not exercised by evals. `StageInput::Document` is a spec 3a candidate; the
  reason it is not in 2a is the trigger engine's overlap behavior (`trigger_engine/mod.rs:511-537`).
  Cost if wrong: trigger plumbing is untested by evals; both arms share it.
- 2026-09-21, for `m6a`: the `structure.rs` entry for `Proofs/Optimization.lean` stays a `Gap`
  naming the lib unit-test consumer, because `structure.rs` has no form for that consumer kind.
  Accepted as the coordinator ruled. The Lean review minors (`import Proofs.Eval` nominal;
  `accept_antitone` naming) go in the PR 1 description, not a fix round.
- 2026-09-21, outage rule (from the user): Anthropic is having an outage (status.claude.com; 529/500).
  If two consecutive Opus dispatches fail with a 5xx, the coordinator falls back to Grok 4.7 for that
  implementation task, brief file as the whole interface, and records the fallback in its ledger.
  Reviews stay on Opus when it answers; a task that landed under a Grok-only review gets an Opus
  re-review before the final whole-branch review. The final whole-branch review waits for Opus.

## Completion log

- `m3` complete 2026-09-22: branch `eval/30-monitor-prework` @ `c44cab2c5`, 10 commits, base
  `eval/03-definition` @ `2629d4a11`. Pack `1e79b6d73`; cases in six axis commits plus `edbe5d1a2`;
  inventory `3274f49f9` + `c44cab2c5`. `cargo test -p gents --lib pack` 72/72. 22 rulings in its ledger
  (`gents-eval-definition-monitor/.superpowers/sdd/m3-prework/progress.md`). Not covered by tests:
  ConfigReferences resolution, EventSource::validate, tool-snapshot build, MiniJinja strict render.
  Open items for spec 3a: case container layout; capture `fields` union (`_docID`, `title`, `summary`,
  `payload`); `StageInput::Document`; keyword-matcher brittleness class.
- `m3` trailers rewritten 2026-09-22 (`git rebase -x`, tree identical): head `c44cab2c5` → `0425499b1`.
- M1 final-review fix round rewrites the six eval branches (a structure.rs hunk moves from PR B into
  PR A). The pin `595fd8bc2` stays valid as a commit, but `eval/05-contract`'s tip moves. `m2` and
  `m6a` keep working on the old base; each rebases its stack onto the new `eval/05-contract` tip
  once, at the end of its plan, before anything is pushed.
- `m6a` complete 2026-09-22: stack rebased onto `eval/05-contract` @ `851db85d6`. Heads:
  `optimization/10-lean` `1a7803340` (2 commits: model; `accept_monotone` rename),
  `optimization/11-conformance` `1b4b109aa` (1 commit), `optimization/12-policy` `d0ab985ab`
  (2 commits: `PolicyV2`; fix wave). Validation on the tip: `lake build`, structure/coverage/feature
  conformance tests, `cargo check --workspace --all-targets`, policy 11/11, fmt. Earlier full runs:
  lib 2482/2482; `cargo test -p gents` fails only the two libp2p dial timeouts. Nine rulings in its
  ledger (`.superpowers/sdd/2026-09-21-optimization-policy-and-lean/progress.md`); R7 added
  `max_missing_usage_bp` to `PolicyV2` per spec §4. Deferred items are recorded per PR in the ledger.
- `m2` complete 2026-09-22: 20 commits on four stacked branches over `eval/05-contract` @ `851db85d6`.
  After the orchestrator's Grok-trailer rewrite (tree identical): `eval/10-runner-support` `054eca35d`,
  `eval/11-runner-core` `d3e1acfd8`, `eval/12-runner-loop` `d38c623df`, `eval/13-runner-embedded`
  `028aa4188`. On the head: `--lib eval` 98, canary 3 passed / 1 ignored, workspace check clean,
  `e2e_configurator --no-run` ok. 27 coordinator rulings (`.superpowers/sdd/2026-09-21-eval-runner/`).
  Residuals: R26 (observation-failed arm grades Runtime; must be Infrastructure), three doc items;
  the three fix-wave commits on `eval/13` touch files owned by 10/11/12 (layer map in the ledger) and
  may be redistributed before push. Workspace retained for PR descriptions.
- 2026-09-22 amendment wave: `eval/05-contract` → `00a8ea84b`; rebased heads `eval/06-protected`
  `075e1970b`, `eval/10` `21b899aae`, `eval/11` `b6b44fdca` (+ literal fix), `eval/12` `754165696`
  (+ literal fix: origin carries `breaker_threshold`, completion carries `evidence_digest`),
  `eval/13` `7d1ca360f`, `optimization/10` `a1093a1bb`, `11` `c71911c7f`, `12` `d61ab88cd`. Tip
  checks: eval lib 105, canary 3/1 ignored, workspace check clean.
- New workspaces 2026-09-22, coordinators on `--model opus`: `m3` (plan
  `2026-09-22-eval-definition-and-checks.md`, base `eval/13` @ `7d1ca360f`) and `m6b` (plan
  `2026-09-22-optimization-driver.md`; its first step builds the M6b base by cherry-picking
  `eval/06-protected`, `optimization/01..03` and `optimization/10..12` onto `eval/13` @ `7d1ca360f`
  as branch `optimization/19-base`). M4 waits for spec 4a.
- 2026-09-22, user rule: commit messages carry no `Co-Authored-By` trailer. The 18 finished branches
  were rewritten with a message filter (trees identical; old refs kept under `refs/original`):
  `design/optimization-substrate` `eaca5a940`; `eval/01` `f58a40a84`, `02` `d849e047b`, `03` `116d659e7`,
  `04` `1e6273315`, `05` `6d8e18e4d` (the pin), `06` `fa4059df6`, `10` `25bb761b4`, `11` `63efa986a`,
  `12` `2f006ece4`, `13` `38c95e8de` (the M3/M6b base root), `30` `cd440f686`; `optimization/01`
  `551a4ce9f`, `02` `918ccf21f`, `03` `254444088`, `10` `d6fbc8b61`, `11` `a9401c9d2`, `12` `eca2d79a5`.
  In-progress branches (`eval/40`, `41`, `optimization/19..22`) still sit on the old commits and are
  rebased onto the rewritten bases at the end of their plans; new commits carry no trailer.
- `m3` complete 2026-09-22: Tasks 1–12 done, Tasks 13–14 (live calibration + fix pass) DEFERRED
  pending a live provider (handoff brief `deferred-task-13-14-dispatch.md` in its ledger directory;
  workspace retained). Branches rebased onto the rewritten `eval/13` @ `38c95e8de` and trailer-stripped
  (trees identical): `eval/40-capture` `7754e429a`, `eval/41-checks` `4d7e54d6f`,
  `eval/42-definition` `aa57d0f13`. Gates: lib 2588, definition pack tests, conversion reproduces the
  20 cases byte for byte (30 stages, 188 refs, 8/6/6). Subject digest moved (workspace-root resolver);
  definition pack digest `sha256:7cefb0af…8785`; `comparability_version` 1 PROVISIONAL until
  calibration. Residuals for issues: the gents-cli scenario sidecar closure lacks a `../` check
  (predates this stack; eval sidecars now pass through it); subject README line 36 half stale.
  Unproven until calibration: `requester_did == $trial` end to end.
- M4 fully planned 2026-09-22: spec `2026-09-22-eval-report-and-cli-design.md` (4a + 4b +
  amendments); plan `2026-09-22-eval-report-and-cli.md`, 5 PRs (`eval/50-report` … `54-compare-breakdowns`),
  reviewed once (16 findings, all ruled and applied). Base: M6b's final `optimization/23-promote` tip
  after its fix wave. `m4` starts on `--model opus` when `m6b` reports plan complete, with the user's
  process rules: batch tasks sharing a file or a library call; reviews overlap the next implementer;
  per task only the pertinent tests plus `cargo check -p gents --tests`; the full `cargo test -p gents`
  and `cargo check --workspace --all-targets` once per PR at its gate; `CARGO_BUILD_JOBS=8`; no trailers.
- `m6b` complete 2026-09-23. Rebased onto the rewritten `eval/13` @ `38c95e8de` and trailer-stripped
  (trees identical): `optimization/19-base` `8d0ca01ac` (16 cherry-picks), `20-job` `044592aec`,
  `21-proposer` `6866f8769`, `22-driver` `1f08a7db3`, `23-promote` `6164d34d6` (promote/revert, live
  tests, the final fix wave incl. the captures fallback F-1). Gates: lib 2675, driver 48 ×3, canary
  3/1, conformance 269/269, workspace check clean; full `cargo test -p gents` fails only the two libp2p
  timeouts. Artifacts in `.superpowers/sdd/2026-09-22-optimization-driver/` (ledger, gate reports,
  final review, `pr-descriptions.md` with the layer map for redistributing the fix-wave commits at
  push). **M4 base: `optimization/23-promote` @ `6164d34d6`.**

## Branch names (renamed 2026-09-23; user rule: `feat/`, `test/`, `perf/`, no internal milestone names or numbered prefixes)

| Old | New | Stack |
|---|---|---|
| `design/optimization-substrate` | `feat/eval-optimization-design` | design docs |
| `optimization/01-lean` · `02-conformance` · `03-cas` | `feat/guarded-publication-lean` · `-conformance` · `-cas` | Track 0 |
| `eval/01-lean` · `02-conformance` · `03-definition` · `04-runtime-collections` · `05-contract` · `06-protected` | `feat/eval-outcome-lean` · `feat/eval-outcome-conformance` · `feat/eval-definition-collection` · `feat/eval-runtime-collections` · `feat/eval-contract` · `feat/eval-protected-collections` | M1 |
| `eval/10-runner-support` · `11-runner-core` · `12-runner-loop` · `13-runner-embedded` | `feat/eval-runner-support` · `-core` · `-loop` · `-embedded` | M2 |
| `optimization/10-lean` · `11-conformance` · `12-policy` | `feat/optimization-policy-lean` · `-conformance` · `feat/optimization-policy` | M6a |
| `optimization/20-job` · `21-proposer` · `22-driver` · `23-promote` | `feat/optimization-job` · `-proposer` · `-driver` · `-promote` | M6b |
| `optimization/19-base` | unchanged, local only; disappears when the MVP chain is linearized | M6b base |
| `eval/30-monitor-prework` | `test/monitor-eval-cases` | M3 pre-work |
| `eval/40-capture` · `41-checks` · `42-definition` | `feat/eval-stage-captures` · `feat/eval-mailbox-checks` · `test/monitor-findings-definition` | M3 |
| `eval/50-report` · `51-cli` · `52-optimization-cli` · `53-watch-gc` · `54-compare-breakdowns` | `feat/eval-report` · `feat/eval-cli` · `feat/optimization-cli` · `feat/eval-watch-gc` · `feat/eval-compare-breakdowns` (renamed when `m4` completes) | M4 |

Planned MVP chain (one linear stack, after `m4` completes): Track 0 → M1 → M2 → M6a → M6b → M4,
each PR targeting its parent; M3 stays a side branch off the runner.

## Workflow amendments (2026-09-23, user-approved; bind every coordinator from now on)

1. **Review by reading.** The implementer's report carries its test log and commit hash. A reviewer
   re-runs only a test it suspects (vacuous, wrong assertion), never the task's whole set, never a
   workspace check.
2. **Two worktrees per PR, ping-pong.** Batch N+1's implementer works in worktree B while batch N's
   reviewer reads in worktree A; both worktrees set `CARGO_TARGET_DIR` to one shared directory
   (`<worktree A>/target`) so nothing builds cold. Create B with `make worktree` from the same base
   and rebase it onto A's tip before each batch.
3. **Small fixes land unreviewed.** A fix diff under ~30 lines confined to what the finding named is
   committed and ledgered without a re-review; larger fixes get a read-only scoped re-review.
4. **Builds only at gates.** The full `cargo test -p gents` (and `-p gents-cli` where touched) plus
   `cargo check --workspace --all-targets` run at a PR's gate and at the final gate only. Per task:
   the pertinent tests plus `cargo check -p <crate> --tests`.
5. **Final review stays**, with one read-only re-review of its fix wave and one gate.
- `m4` complete 2026-09-23: 77 commits over `feat/optimization-promote` @ `6164d34d6`. Branches renamed:
  `feat/eval-report` `65700ca35`, `feat/eval-cli` `f82565639`, `feat/optimization-cli` `457507edc`,
  `feat/eval-watch-gc` `0b81d7818`, `feat/eval-compare-breakdowns` `ec44e41f6` (tip holds the final
  fix wave, 14 commits tagged by PR). Final gate: `cargo test -p gents` 3336/0; `-p gents-cli` 1107
  passed / 20 known-environmental; workspace check clean; no trailers. 57 rulings in
  `.superpowers/sdd/2026-09-22-eval-report-and-cli/`; `pr-descriptions.md` has the five PR texts, the
  layer map and follow-ups. Temp worktree `eval/54b-compare-cli` removed. Pre-push: replace the
  user-visible "(M5)"/"M7" strings and plan-id doc comments; then linearize the MVP chain.
  Ruling: fix-wave commits stay at each stack's tip PR (a scratch trial of redistributing M4's
  found hand conflicts); PR descriptions carry the layer maps.

## The MVP chain (linearized 2026-09-23; 160 commits over `main` @ `0deb7659c`, verified linear, no trailers)

| # | Branch | Head | Parent |
|---|---|---|---|
| 1 | `feat/guarded-publication-lean` | `551a4ce9f` | `main` |
| 2 | `feat/guarded-publication-conformance` | `918ccf21f` | 1 |
| 3 | `feat/guarded-publication-cas` | `254444088` | 2 |
| 4 | `feat/eval-outcome-lean` | `6a9f266e6` | 3 |
| 5 | `feat/eval-outcome-conformance` | `11b3d7b0c` | 4 |
| 6 | `feat/eval-definition-collection` | `669b48935` | 5 |
| 7 | `feat/eval-runtime-collections` | `f3334e3fc` | 6 |
| 8 | `feat/eval-contract` | `be0ec3123` | 7 |
| 9 | `feat/eval-protected-collections` | `21d6b6c59` | 8 |
| 10 | `feat/eval-runner-support` | `227a0f6b2` | 9 |
| 11 | `feat/eval-runner-core` | `b19edaa6a` | 10 |
| 12 | `feat/eval-runner-loop` | `e85fb5824` | 11 |
| 13 | `feat/eval-runner-embedded` | `7f950985e` | 12 |
| 14 | `feat/optimization-policy-lean` | `099c14980` | 13 |
| 15 | `feat/optimization-policy-conformance` | `4bac1f652` | 14 |
| 16 | `feat/optimization-policy` | `ecfd69276` | 15 |
| 17 | `feat/optimization-job` | `801ad0347` | 16 |
| 18 | `feat/optimization-proposer` | `446661c94` | 17 |
| 19 | `feat/optimization-driver` | `20311420c` | 18 |
| 20 | `feat/optimization-promote` | `f357c9603` | 19 |
| 21 | `feat/eval-report` | `b2085ef1b` | 20 |
| 22 | `feat/eval-cli` | `facdab9d8` | 21 |
| 23 | `feat/optimization-cli` | `e50bc198e` | 22 |
| 24 | `feat/eval-watch-gc` | `b352a35aa` | 23 |
| 25 | `feat/eval-compare-breakdowns` | `b43d5085f` | 24 (tip; includes the milestone-string fix) |

Side branches (off the chain): `test/monitor-eval-cases` `46962dfab` on 6; `feat/eval-stage-captures`
`d5472672f` → `feat/eval-mailbox-checks` `e334f960a` → `test/monitor-findings-definition` `c9d2c83cc` on 13.
`optimization/19-base` is obsolete and local only. Chain-tip gates: fmt; `cargo test -p gents` 0
failures (one `e2e_triggers` runtime-snapshot timeout on the first run, passed on rerun: a flake to
file); `-p gents-cli` only the 20 known environmental; conformance 269/269; workspace check; `lake
build` at the eval-core and policy tips. Report: `.superpowers/sdd/mvp-chain/report.md`.

## The MVP stack, squashed (2026-09-23; six PRs, 25 commits; every commit tree identical to its former PR; tip tree identical to `b43d5085f`)

| # | Branch | Head | Commits | Parent |
|---|---|---|---|---|
| 1 | `feat/guarded-publication` | `00043f57d` | 3 | `main` |
| 2 | `feat/eval-contract` | `9a987a43d` | 6 | 1 |
| 3 | `feat/eval-runner` | `df979f0bd` | 4 | 2 |
| 4 | `feat/optimization-policy` | `0f38ea41e` | 3 | 3 |
| 5 | `feat/optimization-driver` | `4ae0ea4a9` | 4 | 4 |
| 6 | `feat/eval-cli` | `cfc7eae7e` | 5 | 5 |
| side | `test/monitor-findings-eval` | `e33f47bb2` | 3 | 3 (the monitor pre-work unit is superseded by the definition unit and dropped) |

The 29 former branches are kept locally as `archive/<old name>` and deleted on the remote at push.
Each squashed commit's body lists the subjects it folds. Report: `.superpowers/sdd/mvp-chain/squash-report.md`.

## `eval-init` coordinator (spun up 2026-09-23, evening)

| Workspace | Coordinator | Plan | Base | Worktree |
|---|---|---|---|---|
| `wB` (label `eval-init`), agent `wizard` in pane `wB:p1` | Claude, `claude --model opus` | `plans/2026-09-23-eval-init-wizard.md` (spec `specs/2026-09-23-eval-init-wizard-design.md`, brief `orchestration/2026-09-23-eval-init-wizard-brief.md`) | `feat/eval-cli` @ `cfc7eae7e` | `~/Repos/Source/gents-eval-init-wizard`, branch `feat/eval-init-wizard` |

Binding rules for it: the standing rules above with rule 3 corrected (no `Co-Authored-By` trailers,
ever) and the 2026-09-23 workflow amendments. Ledger under the design worktree at
`.superpowers/sdd/2026-09-23-eval-init-wizard/`. Delivers PR 7 of the stack, on #1658.

### `eval-init` rulings relayed 2026-09-23 (evening)

- R1 a pilot is one run per populated split; `PilotOutcome.run_ids`. R2 the loader round trip is
  validation step 7: `write.rs` has `stage()` and `commit()`, and a round-trip failure returns to
  the author as a validation round. R3 the README records pilot run ids on "keep" and notes a
  revision. R4 an existing `--out` without `--force` is refused before the interview. R5
  `InitOutcome` carries the assembled definition for the pilot.
- Speed-up the coordinator reports as approved by the user in its own pane: Tasks 4 and 5 in
  worktree B with its own target dir, in parallel with Tasks 1 to 3 in A, cherry-picked back;
  `CARGO_BUILD_JOBS=5` per workspace; Tasks 8 and 9 as one dispatch; Task 4 without a task review.
  Orchestrator's conditions: fall back to the shared-target amendment on any OOM, timeout or 2×
  slowdown; the final reviewer reads the authoring contract against spec §3 line by line;
  cherry-picks stay trailer-free.
- 2026-09-24: `eval-init` Tasks 1 to 3 complete, review clean, `feat/eval-init-wizard` @ `15b2c76a5`
  (4 commits on `cfc7eae7e`). Fix round 1: `gents eval checks` answers before `EvalContext::resolve`
  (needs no home); the runner's `validate_purpose` now accepts `pilot` (plan defect: Task 3 named
  exposure and compare but not the freeze-time purpose check). Build fallback triggered: B's target
  was cold, so B uses A's shared target, one build at a time. Task 4 committed in B (`1a07834bc`)
  with a fix: its worked example had invented a check name and now uses registered checks only.
  Tasks 5 to 7 run as one dispatch in A. Minors deferred to the final review, ledgered.
- 2026-09-24: `eval-init` Tasks 4 to 7 complete, reviews clean, `feat/eval-init-wizard` @ `6a6cec8c9`
  (10 commits). Task 4 cherry-picked from B (`6e0c6b60c`, `7e8c82019`). Rulings: the worked example
  uses two stages with one `captured_rows_count` each (a stage refuses a duplicate check name);
  case sidecars are `cases/<id with - → _>.json` (loader asset names are snake_case, matching the
  monitor pack); the dossier escapes `$` so placeholders stay markers (the loader substitutes
  defaults, contrary to the plan); README subject line threaded in Task 8; capture filter keys are
  held to the collection's fields; `jsonschema` is a normal gents-cli dependency. Fix round: scratch
  node shut down on every path. Tasks 8 and 9 dispatched as one unit. Orchestrator asks the final
  reviewer to pin the `$` escaping in the golden test and to check the filter-key message names key
  and collection.
- 2026-09-24: `eval-init` PLAN COMPLETE. `feat/eval-init-wizard` @ `e9f3b515b`, 17 commits on
  `cfc7eae7e`, clean, trailer-free. Gate at `e2fca53b8`: fmt clean; gents 3342/0; gents-cli 1148
  passed + 20 known environmental; workspace check clean. Final review "with fixes": three
  Important (prompt's reducer semantics; validation floor, a plan defect; the live smoke) and one
  must-fix (printed commands dropped `--home`/`--graphql`), all addressed in a 3-commit fix wave;
  re-review clean. Runner accepts purpose `pilot`; `eval run --purpose pilot` refused; `eval checks`
  needs no home. 33 rulings in the ledger. Worktree B removed by the orchestrator. Parked minors
  ledgered with file:line.
- 2026-09-24 CI pass over the stack: `rust-and-cli (desktop)` failed on #1654 to #1658 because the
  committed desktop TypeScript bindings lacked the `Eval*` types (`typegen::committed_bindings_match_regeneration`);
  `repository-guard` failed on #1716 because the `eval_author` README had no `## ` section. Fixes:
  `31cc751a7` (bindings, on `feat/eval-contract`), `e5a9b4363` (bindings for `EvalFixtureFile`, on
  `feat/eval-cli`), `86f1cb169` (README). Stack rebased: runner `75556ded9`, policy `b146bc35e`,
  driver `3becb1e31`, cli `e5a9b4363`, side `a9691be48`, wizard `524843870`. Force-pushes are the
  user's. `main` itself is red on `rust-and-cli (cli)` in four of its last five runs, unrelated.

## `rebase` coordinator (spun up 2026-09-24)

| Workspace | Coordinator | Brief | Worktree |
|---|---|---|---|
| `wC` (label `rebase`), agent `rebaser` in pane `wC:p1` | Claude, `claude --model opus` | `orchestration/2026-09-24-stack-rebase-brief.md` | `~/Repos/Source/gents-rebase` on scratch branch `rebase/stack-onto-main` from `feat/guarded-publication` @ `10f64b5bb` |

Cause: a maintainer (jackzampolin) force-pushed `feat/guarded-publication` at 13:57 UTC, rebasing
PR 1 onto `main` @ `8059b672d` (236 commits past our base). Every other branch descended from the
old tip `00043f57d`; GitHub registered no check runs for the heads pushed today. The coordinator
rebases contract, runner, policy, driver, cli, wizard and the side branch in order, resolves the
known conflicts (conformance snapshot and ledger, lean vocab support, conformance consumers, test
support, the wizard's chat visibility and Cargo.toml), regenerates desktop bindings where the
generated commits conflict, and gates each tip against main's new CI rules. Ledger under the design
worktree at `.superpowers/sdd/2026-09-24-stack-rebase/`. The contract and wizard worktrees were
removed first (clean, pushed) so every branch is free to check out.
