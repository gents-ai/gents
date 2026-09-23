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
