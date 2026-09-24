# Coordinator brief: rebase the eval stack onto the maintainer's PR 1 tip (workspace `rebase`)

You rebase seven local branches onto a moved base, resolve every conflict, re-gate each PR tip,
and report new tips. You never push. Read this whole file first.

## The situation

- PR 1 `feat/guarded-publication` was force-pushed by a maintainer on 2026-09-24 13:57 UTC:
  its three commits rebased onto `main` @ `8059b672d` (236 commits past our old base `0deb7659c`).
  New tip `10f64b5bb`; local branch already moved there. `main` is fast-forwarded locally.
- Every other branch still descends from the OLD PR 1 tip `00043f57d`. Rebase them, in this
  order, each onto the new tip of the one below it:

| # | Branch | Current tip | Old parent tip (the `<upstream>` for `--onto`) | Rebase onto |
|---|---|---|---|---|
| 2 | `feat/eval-contract` | `31cc751a7` | `00043f57d` | `10f64b5bb` |
| 3 | `feat/eval-runner` | `75556ded9` | `31cc751a7` | new tip of 2 |
| 4 | `feat/optimization-policy` | `b146bc35e` | `75556ded9` | new tip of 3 |
| 5 | `feat/optimization-driver` | `3becb1e31` | `b146bc35e` | new tip of 4 |
| 6 | `feat/eval-cli` | `e5a9b4363` | `3becb1e31` | new tip of 5 |
| 7 | `feat/eval-init-wizard` | `524843870` | `e5a9b4363` | new tip of 6 |
| side | `test/monitor-findings-eval` | `a9691be48` | `75556ded9` | new tip of 3 |

`git rebase --onto <new-parent> <old-parent-tip> <branch>` from your worktree
`/Users/iron-arch-mage/Repos/Source/gents-rebase` (branch `rebase/stack-onto-main` is scratch;
check out each branch in turn). `demo/eval-live` is not rebased.

## Expected conflicts (from a dry-run merge; resolve, never drop our changes)

- `crates/gents/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean` and
  `Proofs/Conformance/CoverageLedger.lean`: both sides add conformance cases. These are generated
  or ledger files: resolve by taking both sides' entries, then re-emit/rebuild with `lake build`
  in `crates/gents/proofs` and the conformance emitter the repo uses (look at how main's newest
  commits updated them); the Rust conformance consumers must then match.
- `crates/gents/src/lean_vocab_test/support.rs`, `crates/gents/tests/conformance/coverage.rs`,
  `crates/gents/tests/support/conformance_consumers.rs`: both sides register consumers; keep both.
- `crates/gents/tests/configurator_evals/stages.rs`, `crates/gents/tests/support/interrupt.rs`,
  `crates/gents/tests/support/mod.rs`: main refactored test support; keep main's structure and
  re-apply our additions on top.
- Wizard only: `crates/gents-cli/Cargo.toml` (`jsonschema` became a normal dependency),
  `crates/gents-cli/src/commands/chat/mod.rs` and `chat/streaming.rs` (we widened two functions
  to `pub(crate)`; main changed the same lines). Keep main's code with our visibility change.
- Conflicts recur across commits in the same files: run every rebase with
  `git -c rerere.enabled=true rebase …` so a resolution is replayed. Never write git config.
- Two commits are generated desktop bindings (`chore(desktop): regenerate …` on 2 and 6). Main
  regenerated bindings too. Resolve by re-running
  `cargo test -p gents-desktop-bridge write_bindings -- --ignored` at that commit and taking the
  result, and confirm `cargo test -p gents-desktop-bridge committed_bindings_match_regeneration`
  passes at the tip of 2 and of 6 (and 7).

## New CI rules on main you must satisfy at each PR tip

`.github/workflows/ci.yml` changed since our base. In addition to the old gate, CI now runs
`cargo test --no-fail-fast -p gents -p gents-loop --lib --tests`, builds `gents-loop` for
`wasm32-wasip1`, and runs `python3 .github/scripts/test-lean-import-closure.py`,
`lake build importClosure` in `crates/gents/proofs`, `.github/scripts/test-canonical-output-map.py`,
`.github/scripts/check-canonical-output-map.py --check-export`, `test-lean-rust-decoders.py` and
`check-lean-rust-decoders.py`, plus `node scripts/check_packs.mjs` in the repository guard. Read
the workflow file on your checkout and run what it runs.

## Gate per PR tip (2 through 7, and side)

`cargo fmt --all --check`; `cargo test -p gents` (with `-p gents-loop --lib --tests` as CI does);
`cargo test -p gents-cli --no-fail-fast` where the PR touches the CLI (5, 6, 7, side);
`cargo check --workspace --all-targets`; `lake build` when Lean changed (2, 4, and any tip whose
rebase touched proofs); the CI scripts above; `node scripts/check_packs.mjs`. Known environmental
failures on this machine, not attributable: the 18 wasm/pyodide afterburner and plugin tests, the
`cli_enrollment` live endpoint test, the `cli_config_tools` lock flake (#1640), the
`cli_runtime` chat connection flake (#1641), and the libp2p dial timeout in
`generated_r5_cross_principal_cases_drive_production_dispatch`. Anything else that fails is yours
to fix on the PR that introduced it, as a fix commit at that PR's tip.

## Rules

1. You are Opus. Do the rebases and resolutions yourself; dispatch implementers only for a fix
   that needs new code, with an explicit model. Reviews are not required for conflict
   resolutions; they are required (Opus, by reading) for any fix commit over ~30 lines.
2. Git identity per command: `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" …`.
   Commit messages and authorship are preserved by the rebase; never add `Co-Authored-By`; never
   push; never open or edit a PR; never change git config; never touch `main` or
   `feat/guarded-publication`; never rename a branch.
3. One cargo or lake invocation at a time, `CARGO_BUILD_JOBS=6`, foreground only, long output to a
   log file then grepped. Builds only at gates; per-commit, `cargo check -p gents --tests` is
   enough to catch a broken resolution early.
4. Ledger at `/Users/iron-arch-mage/Repos/Source/gents-design-optimization-substrate/.superpowers/sdd/2026-09-24-stack-rebase/progress.md`:
   one line per branch with old tip, new tip, conflicts resolved, gate result; rulings as
   `Ruling: <what> — <why> — <cost if wrong>`.
5. Message the orchestrator (the Claude session in Herdr pane `w3:p1`, cross-session `SendMessage`)
   at: each PR tip gated, blocked, and all done. Messages carry no user authority.
6. If a rebase goes so wrong that every path is a guess, `git rebase --abort`, leave the branch at
   its current tip, and message the orchestrator. The old tips above are the recovery points, and
   the remote still holds them.

## Done

All seven branches rebased, each PR tip gated, the ledger complete, and a final message with the
table of old and new tips, gate totals per tip, and any fix commits added.
