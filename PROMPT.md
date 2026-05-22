# Desktop shell post-panel-wave smoke fixes

You are on branch `fix/desktop-shell-smoke` (worktree at
`/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-fix-desktop-smoke`).

This is an **accumulating fixes branch**, not a single feature. The
desktop panel wave just landed:

- #322 — R5 subagent lineage view
- #324 — command-policy denial inline render
- #326 — backend health panel
- #327 — backgrounded tools panel
- #328 — MCP health status (DefraDB schema + write path + panel)
- #325 — interrupt/cancel UX bundle (button + cause badges + cascade)

The user is smoke-testing the integrated desktop app and will hand you
bug reports as they find them. Land each fix on this branch.

# Working pattern

For each user-reported bug:

1. **Reproduce or read the report carefully.** If the symptom is
   ambiguous, ask one clarifying question before diving in. Don't
   speculate.
2. **Find the minimal fix.** Tightly scoped to the bug. Don't drive-by
   refactor, don't rename, don't reshape adjacent code. CLAUDE.md's
   "don't add features beyond what the task requires" applies here
   strongly.
3. **Write a test where reasonable.** A vitest test for a React
   regression, a Rust unit test for a bridge regression. If the bug is
   purely visual/CSS, no test — just verify in the browser.
4. **Single atomic commit per bug.** Each commit should be revertable
   in isolation. Conventional subject: `<area>: <what>` (e.g.,
   `transcript: CancelCauseBadge renders empty string on missing at`,
   `operations: backend-health panel crashes when no backends
   registered`). Mention the symptom + root cause + fix in 2–3 body
   lines.
5. **Push immediately after each commit.** The user is watching the
   PR; let them see fixes land. First push opens the PR titled
   "Desktop shell post-panel-wave smoke fixes" with the
   below-the-fold list. Subsequent pushes update it.

# Discipline

- **DO NOT touch Lean state machines** (`crates/defra-agent/proofs/`)
  or any file that imports from them. Per CLAUDE.md, anything that
  changes "what transitions are legal or what invariants hold" starts
  in Lean. If a bug requires that, STOP and report — file a separate
  issue, don't smuggle it into a "fix".
- **DO NOT modify the formal ledger** (`CoverageLedger.lean`) or
  `conformance_consumers.rs` to silence a drift test. If the drift
  test fires, the surface binding is wrong — STOP and report.
- **DO NOT rebase this branch yourself** unless asked. The user will
  rebase + merge once the smoke session reaches a stopping point.
- **DO write fixes that touch only what's necessary.** A 1-line CSS
  fix is a 1-line diff. Don't refactor surrounding code.
- **DO update tests if a bug fix changes assertions.** If a
  pre-existing vitest assertion was wrong about expected behavior,
  update it in the same commit as the fix and explain why in the
  commit body.

# When to STOP and report

- The bug requires changing the Lean spec or a state-machine
  transition rule
- The bug is in shared runtime code (`crates/defra-agent/src/`) and
  the fix would affect more than the desktop reading path
- The bug crosses multiple panels in a way that suggests an
  architectural issue — file a separate issue rather than a sprawling
  fix
- The "fix" feels too big to be revertable as a single commit

In these cases: leave the working tree clean, summarize the finding,
and let the user decide (file an issue, dispatch a separate worktree,
etc.).

# Verification after each fix

Lightweight per fix (run only what's relevant):

- TS/React regression: `cd apps/desktop-tauri && npx tsc --noEmit && npx vitest run <relevant-test>`
- Rust bridge regression: `cargo check -p defra-agent-desktop-tauri`
  and `cargo test -p defra-agent-desktop-tauri <relevant-test>`
- CSS/visual: spot-check in the dev app (the user will tell you if it
  still looks wrong)

Heavyweight (only if the fix touched broadly):

- `cargo test -p defra-agent` (~5 min)
- `cd apps/desktop-tauri && npm test` (~30s, all vitest)
- `cd crates/defra-agent/proofs && lake build` (only if you somehow
  touched a Lean file — which you shouldn't be)

# Out of scope

- New features
- Refactoring
- Lean spec changes
- Adding panels
- Reformatting (unless it's the same line you're already editing)
- The pre-existing startup_recovery flake tracked at #330 — not yours
