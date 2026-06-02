# Lean Verification Sweep Audit

Issue: #337
Date: 2026-06-02

## Scope

This audit checked the issue set from `PROMPT.md` with the spec-first rule from
`CLAUDE.md`: Lean proof coverage first, generated conformance consumers second,
runtime implementation changes only when the spec/test path exposes a mismatch.

The P2P request/response pairing under partition remains out of scope for this
sweep. It belongs with #155 and the remaining #349 P2P checkbox.

## Classification

| Surface | Classification | Existing coverage | Action in this sweep |
| --- | --- | --- | --- |
| Request lifecycle transitions | Atomic edge | `Proofs/Conformance/ContractCases/LifecycleTransitions.lean`; `tests/state_machine_conformance/request_lifecycle.rs` | No new gap found |
| Streaming response transitions | Atomic edge + composed interrupt flow | `Proofs/StreamingResponse/*`; `streaming_response_interrupt_flow_cases`; daemon consumer in `state_machine_conformance` | No new gap found |
| Interrupt workflow | Composed workflow | Request terminalization, streaming response interrupt finalization, queued wakeup drain, and daemon interrupt-flow consumer | No new gap found |
| Deadline workflow | Trace + boundary metadata | Queue deadline cases, request transition cases, managed-exec/native filesystem deadline tests | No new gap found |
| Startup recovery | Trace + runtime sweep | `Proofs/Recovery/Sweeps/*` emits 19 cases consumed by `generated_recovery_sweep_cases_drive_startup_recovery_contract` | No new gap found for current startup sweep semantics |
| Tool cancel | Trace + runtime workflow | Tool call lifecycle contract, cancel cause vocabulary, cascade/detach integration tests | No new gap found for current single-deployment and bridge semantics |
| Child-process cancel | Runtime workflow + managed-exec proof | `Proofs/ManagedExec/*`, native filesystem preemption boundary, managed-exec liveness cases | No new gap found |
| Retry / manual reissue | Trace + runtime workflow | `Proofs/SessionRecovery.lean`; generated session recovery cases consumed by DB-backed reissue tests | No new gap found |
| Provider failure | Atomic edge + runtime admission policy | Request fail edges, inference terminal reasons, backend health admission cases | No new gap found |
| Trigger CLI dispatch | Runtime workflow | Merged PR #308 added `config_task_run_matches_lean_manual_dispatch_contract` for `triggers/operatorCli` | #282 can be closed |
| CodexShim turn lifecycle | Projection examples only | Projection cases existed, but no terminal coherence, turn-lifecycle monotonicity, or local-interrupt eligibility proof | Fixed in this sweep |
| ApplyReconcile delete | Tracker only | Current model intentionally has no `delete` constructor | Keep #57 open until live-only removal is requested |

## Fixes Landed In This Branch

CodexShim had the only new proof gap found during this pass that was both
in-scope and not already covered by generated consumers.

This branch adds:

- `CodexShim.codex_turn_terminates_precisely`: projected terminality is exactly
  request terminality, local interrupt acknowledgement, or terminal response
  status.
- `TurnPhase.lexOrd` and `CodexShim.turn_lifecycle_never_regresses`: valid
  Codex-facing turn transitions do not regress.
- `CodexShim.local_interrupt_requires_interruptible` and
  `CodexShim.local_interrupt_shortcut_sound`: local interrupt acknowledgements
  are only sound for `processing` or `inputRequired` observations and project to
  terminal `interrupted`.
- Generated CodexShim projection rows now expose effective terminality and
  interruptible-state bits.
- New generated CodexShim turn-lifecycle rows are registered in the coverage
  ledger and consumed by the existing `state_machine_conformance` CodexShim test.

## Remaining Work

Remaining formal work should stay on the dedicated tracking issues:

- #349 gap 1: startup recovery sweep semantics are covered for the current
  implemented recovery operations. A stronger "same as uninterrupted execution"
  theorem for arbitrary in-flight tool results remains broader than the current
  startup sweep model.
- #349 gap 2: depth bounds, subagent source recovery, bridge cascade, and
  cross-deployment cases are covered by existing Lean/Rust consumers. A general
  arbitrary delegation graph termination/acyclicity proof remains open.
- #349 gap 3: P2P request/response pairing under partition is intentionally out
  of scope for this Lean sweep.
- #57: delete semantics remain tracker-only until live-only document removal is
  a product requirement.

## Verification

- `cd crates/defra-agent/proofs && lake build`
- `cargo test -p defra-agent`
- `cargo test -p defra-agent generated_codex_shim_projection_cases_pin_adapter_mapping -- --exact`
- `cargo test -p defra-agent-cli config_task_run_matches_lean_manual_dispatch_contract -- --exact`
- `cargo fmt --all`
- `git diff --check`
