# Repository maintenance pack

This self-contained pack performs a whole-repository, behavior-preserving cleanup round. It follows the code-review pack's durable graph, but scans the current tree and recent history, plans focused work units, and executes them as an ordered commit series rather than stopping at recommendations:

```text
MaintenanceJob -> recon -> N MaintenanceArea scanners
               -> MaintenanceCandidate + MaintenanceScanResult
               -> adversarial verifier -> MaintenanceVerdict + MaintenanceVerificationSummary
               -> commit planning -> MaintenanceFinding + MaintenanceWorkPackage + MaintenanceReport
               -> one execution owner commits the ordered plan in one isolated worktree
               -> MaintenanceExecutionResult + MaintenanceExecutionSummary barrier
               -> review + CI repair loop -> one MaintenancePullRequest
```

Recon, scanning, verification, and commit planning are read-only. The round creates one sibling worktree and one branch. Each work package contains one to three verified findings and becomes one focused commit. A single execution owner reads the closed package ledger and executes it in numeric order, because package document arrival order is not an execution callback. Only its count-balanced completion barrier can start publishing. A terminal agent runs the checked-in review pack, adds focused safeguard commits for confirmed findings, opens one normal GitHub PR, watches required checks, and performs bounded CI repairs. Long local gates and CI waits are polled rather than assigned short wall-clock deadlines. It never merges the PR.

## Stable maintenance categories

The five mandatory categories come from six recurring cleanup waves in this repository between April and July 2026:

1. dead code, dependencies, assets, compatibility paths, and unwired scaffolding;
2. duplicate helpers, pathways, fixtures, tests, and canonical-owner drift;
3. hollow, false-green, flaky, stale, or exactly redundant tests;
4. oversized or mixed-responsibility files that have cohesive extraction seams;
5. narration, stale implementation history, duplicated documentation, and comment/contract drift.

Recon may add narrow repository-specific categories, but cannot replace the mandatory five.

## Run it

```bash
make maintain
make maintain MAINTENANCE_PROMPT='Focus on CLI and runtime ownership seams'
make maintain MAINTENANCE_AREAS=7 MAINTENANCE_HISTORY_DEPTH=400
make maintain MAINTENANCE_KEEP_HOME=1 MAINTENANCE_JOB_ID=cleanup-2026-08
```

`MAINTENANCE_ROOT` defaults to the current repository and `MAINTENANCE_WORKTREE_PARENT` to its parent directory. The parent is the operator tool ceiling so execution and review can use the sibling worktree; read-only stages remain explicitly rooted at the source repository. `MAINTENANCE_HEAD` defaults to `HEAD` and `MAINTENANCE_PR_BASE` to `main`. History identifies prior cleanup patterns and avoids reopening merged work; it does not restrict findings to a diff. Automatic runs use 5-10 areas. The usual provider/profile controls mirror `make review` with a `MAINTENANCE_` prefix.

Every run lands under `demo/repo-maintenance/runs/<job-id>/`. `results.json` contains the report, confirmed findings, commit plan, execution ledger, and terminal PR status. A zero-finding run emits one no-safe-work sentinel and records `skipped` without creating a worktree or PR. `green` means the final review has no confirmed findings and every required GitHub check succeeded; all other terminal states retain exact evidence.

## False-positive policy

Counts are routing signals, not findings. A scanner must prove reachability and ownership before deleting code, and must preserve feature-gated/generated/public/serialization/GraphQL/FFI/reflection/compatibility surfaces, formal and conformance contracts, observability, operator guidance, rationale, safety arguments, and intentionally distinct boundary tests.
