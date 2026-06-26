# PROMPT — #554: couple Lean build + Rust conformance into one CI contract gate

> Worktree kickoff brief. Part of the Lean-model review umbrella **#551**.
> Issue: **#554**. CI/infra work — mostly `.github/workflows/ci.yml`.

## The problem (validated)

In `.github/workflows/ci.yml` the `lean-proofs` job and the `rust-and-cli` job
are **independent** — neither declares `needs:` on the other, and they run on
different runners. A Lean model change can land green if `lake build` succeeds
while Rust conformance (which encodes the Lean→Rust contract) runs separately
against a stale/consistent-by-luck mapping.

Anchors (this branch, `ci.yml`):
- `:25` — `rust-and-cli:` job.
- `:99` — `Run PR proof conformance tests` (`cargo test -p defra-agent --test conformance`, `pull_request` only).
- `:103` / `:107` — full library/integration + CLI suites (non-PR).
- `:123` — `lean-proofs:` job (self-hosted macOS ARM "studio" runner).
- `:136` — `Build proofs` (`lake build`).
- No `needs:` anywhere coupling the two.

The coverage-ledger test
(`lean_contract_coverage_ledger_accounts_for_every_emitted_domain`) only checks
that every emitted domain **resolves to a registered consumer** — existence and
name-resolution — **not** that the consumer exercises a representative
production code path. See `crates/defra-agent/tests/support/conformance_consumers.rs`
and `crates/defra-agent/tests/conformance/coverage.rs`.

## Why it matters

Per CLAUDE.md, the value of the formal core is highest when "Rust cannot change
lifecycle semantics without breaking Lean-backed conformance." Today the two
halves are not gated together, so that property isn't enforced in CI.

## Proposed work

1. **Couple the gate.** Make the conformance/contract job depend on (or run
   after) a successful `lake build`, so the emitted JSON is regenerated from the
   **just-built** proofs and a Lean change cannot be green-while-stale. Options:
   - add `needs: lean-proofs` to the conformance job and have it consume the
     built proofs, or
   - run `lake build` + the conformance/ledger tests in one job.
   The Rust conformance helper already runs `lake build Proofs.Conformance.Contracts`
   + `lake env lean --run` (`crates/defra-agent/src/lean_vocab_test.rs`), so the
   coupling is about **gating**, not plumbing.
2. **Run the coverage-ledger + conformance tests in the same gate** that builds
   the proofs.
3. **(Stretch)** Strengthen the ledger from "consumer resolves" toward "consumer
   exercises a runtime path" — distinguish enum/string-equivalence consumers from
   state-machine-driving consumers. Likely a follow-up; scope-check before
   committing to it here.

## Definition of done

- A Lean spec change that invalidates the contract **fails CI** (demonstrate
  with a temporary breaking tweak, confirm red, revert).
- Conformance runs on the same trigger as the Lean build, not a disjoint path
  (mind the current PR-only vs main-only split at `:99`/`:103`).
- No new flakiness — see the known parallel-build flake below.

## Watch out

- **Known flake**: `defra-agent-cli` lean_apply_write_boundary / identity_decide
  tests transiently fail when `lake build` races `rustc`. If you put Lean build
  and Rust in the same job, **don't let them race** — sequence them. Rerun in
  isolation before concluding main is broken.
- Self-hosted runners: `lean-proofs` is on `[self-hosted, macOS, ARM64, studio]`.
  Don't assume a hosted ubuntu image is available for a merged job.
- GH "re-run failed jobs" keeps the original merge commit — if main moved, use
  `gh pr update-branch` rather than trusting a stale re-run.
