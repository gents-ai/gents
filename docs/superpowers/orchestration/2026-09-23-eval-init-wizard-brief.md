# Coordinator brief: `gents eval init` (workspace `eval-init`)

You coordinate one plan to completion with `superpowers:subagent-driven-development`. Read this
file first, then the plan, then the spec. Nothing else in the design worktree is yours to edit.

| Item | Value |
|---|---|
| Plan | `/Users/iron-arch-mage/Repos/Source/gents-design-optimization-substrate/docs/superpowers/plans/2026-09-23-eval-init-wizard.md` |
| Spec | `/Users/iron-arch-mage/Repos/Source/gents-design-optimization-substrate/docs/superpowers/specs/2026-09-23-eval-init-wizard-design.md` |
| Worktree A (yours) | `/Users/iron-arch-mage/Repos/Source/gents-eval-init-wizard`, branch `feat/eval-init-wizard`, base `feat/eval-cli` |
| Worktree B (create it) | `make worktree BRANCH=feat/eval-init-wizard-b DIR=/Users/iron-arch-mage/Repos/Source/gents-eval-init-wizard-b BASE=feat/eval-cli` from `/Users/iron-arch-mage/Repos/Source/gents`; set `CARGO_TARGET_DIR=/Users/iron-arch-mage/Repos/Source/gents-eval-init-wizard/target` in both |
| Ledger | `/Users/iron-arch-mage/Repos/Source/gents-design-optimization-substrate/.superpowers/sdd/2026-09-23-eval-init-wizard/progress.md` (create the directory) |
| Orchestrator | the Claude session in Herdr pane `w3:p1`; message it with cross-session `SendMessage` only at: task complete, ruling made, blocked, plan defect, plan complete |
| Side branch to copy from (Task 2) | `git show 617bde8fc -- crates/gents/src/pack/loader.rs crates/gents/src/pack/loader/tests.rs` in the main checkout |

## Rules that bind you

1. **Models.** You are Opus. Dispatch implementers and reviewers with an explicit model: `opus` for
   everything that needs judgment (Tasks 5 to 9 and every review), `sonnet` for transcription-shaped
   tasks (Tasks 2, 4, 10). Never inherit, never Fable. If Anthropic returns sustained 5xx twice on a
   dispatch, fall back to a Grok 4.7 Herdr agent for implementation (`herdr agent start <name>
   --kind grok --pane <sibling>`; its brief file is its whole interface); reviews stay on Opus.
2. **Git.** Commit as `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com"`.
   **No `Co-Authored-By` trailers, ever** (this overrides the older wording in the parallel-coordinators
   doc). Never push, never open a PR, never change git config, never rename the branch.
3. **Build budget.** One cargo invocation at a time per workspace, `CARGO_BUILD_JOBS=4`, foreground
   only: never background, never `sleep`, never poll. Long output to a log file, then grep.
4. **Tests.** Per task: the pertinent tests plus `cargo check -p <crate> --tests`. The full
   `cargo test -p gents`, `cargo test -p gents-cli`, `cargo check --workspace --all-targets` and
   `cargo fmt --all --check` run once, at the final gate (Task 10). The known environmental failures
   (`generated_r5_cross_principal_cases_drive_production_dispatch` libp2p timeout; the 20 gents-cli
   tests listed as environmental in the M4 completion note) are not attributable to this work.
5. **Workflow amendments.** Review by reading (a reviewer re-runs only a test it suspects). Batch
   Tasks 1 to 3 as one dispatch and one review; Tasks 5 to 7 as one dispatch and one review;
   Task 4 alone (Sonnet); Task 8 alone; Task 9 alone; Task 10 is the gate. Small fixes (< 30 lines,
   confined to the finding) land unreviewed and ledgered. One final whole-branch review on Opus,
   one fix wave, one read-only re-review.
6. **Repo gates.** Escape interpolated GraphQL; never `[]` in a mutation; `tracing` not `println!`
   in library code; the CLI writes to its `out` writer. `cargo fmt` clean.
7. **Rulings, not stalls.** Conflicts between plan and spec resolve to the spec; record
   `Ruling: <what> — <why> — <cost if wrong>` in the ledger and continue. Stop only for: a push or PR,
   a destructive operation, or a plan so broken every path is a guess.
8. **Scope.** Edit only inside your two worktrees and your ledger directory.

## What "done" means

Every task complete in the ledger, the final review clean, the gate green (modulo the known
environmental failures), branch `feat/eval-init-wizard` at a tip you name, and a completion
message to the orchestrator with: tip commit, commit count, test totals, the two rebase notes
from the plan's Global Constraints, and any parked findings. Do not push.
