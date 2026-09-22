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
   End every commit message with `Co-Authored-By: <the implementing model> <noreply@anthropic.com>`.
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
| `eval/05-contract` pinned for M6a and M2 | `595fd8bc2` |
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
