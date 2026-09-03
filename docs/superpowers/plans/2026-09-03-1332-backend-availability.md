# #1332 Backend Effective Availability Single Owner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One predicate answers "is this inference backend available to route to right now": `BackendAdmissionConfig::is_available` (mirrors `Proofs.BackendHealth.effectiveAvailable`, B6). In-process readers call it; out-of-process readers (CLI `/healthz`, `fleet-slots`) consume the runtime's published readiness projection instead of re-deriving from the document.

**Architecture:** `InferenceBackend::is_available` (document-only) is deleted. `document_view/snapshot.rs` builds the `BackendAdmissionConfig` first and uses its verdict to classify behaviors as unavailable (with the same three `BehaviorReadinessUnavailableReason`s it produces today, derived from which term failed). `agent/builder.rs` binds the constructed `BackendAdmissionConfig` and consults the live `BackendHealthMap` like the reconciler does. `healthz.rs` and `fleet_slots.rs` compute `backend_degraded`/`accepting_admission` from `project_behavior_readiness_summary` / `BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable`, and `fleet_slots.rs` calls the exported `admission::slot_accounting` functions instead of its copy.

**Tech Stack:** Rust.

**Spec:** GitHub issue #1332. Lean: `Proofs/BackendHealth/{State,Properties}.lean` (B6) already defines the predicate; no proof change.

## Global Constraints

- After this PR, `grep -rn 'probe_status == \|probe_status != \|HEALTHY_PROBE_STATUS' crates --include='*.rs' | grep -v admission/config.rs | grep -v test` returns only document-write sites (the prober promoting `unknown → healthy`) and no availability decisions.
- Measured health stays unpersisted (`backend_health.rs:7` policy); out-of-process readers therefore may not compute availability from `InferenceBackend` rows at all.
- The three readiness reasons (`BackendDisabled`, `BackendTemporarilyUnavailable` for probe-not-healthy, `BackendTemporarilyUnavailable` for measured veto) keep their current messages; assert with the existing readiness tests.
- Net code deletion.

---

### Task 1: In-process callers use `BackendAdmissionConfig::is_available`

**Files:**
- Modify: `crates/gents/src/admission/config.rs` (add `pub(crate) fn availability(&self) -> BackendAvailability { Available | Disabled | ProbeNotHealthy | MeasuredUnhealthy }` so callers can name the failing term without recomputing it; `is_available()` = `availability() == Available`)
- Modify: `crates/gents/src/backend_registry.rs:99-101` (delete `InferenceBackend::is_available`)
- Modify: `crates/gents/src/agent/document_view/snapshot.rs:122-160` (build `BackendAdmissionConfig::from_backend(&backend)?.with_measured_unhealthy(measured_vetoed.contains(..))` once, match `availability()` to produce the same three errors; move the later construction at ~398 up so the config is built once and reused)
- Modify: `crates/gents/src/agent/builder.rs:522-528` (bind the config, apply the builder's `BackendHealthMap` if it has one, gate on `is_available()`)
- Test: existing `document_view` snapshot tests and `builder` tests; add one builder test that a backend vetoed in the health map is rejected.

- [ ] Write the failing builder test; run; implement; run `cargo test -p gents --lib agent::document_view agent::builder admission` green; commit — `runtime: BackendAdmissionConfig is the only backend availability predicate (#1332)`.

### Task 2: Out-of-process readers consume the readiness projection

**Files:**
- Modify: `crates/gents-cli/src/http/healthz.rs:~53` (`backend_degraded` = any behavior readiness row whose unavailable reason is `BackendTemporarilyUnavailable` or `BackendDisabled`, via `project_behavior_readiness_summary` which the same function already calls for `runtime_ready`; delete the `probe_status != "healthy"` literal)
- Modify: `crates/gents-cli/src/http/fleet_slots.rs:~126-136, ~389-402` (delete `accepting_admission`/`normalized_probe_status`; the per-backend "accepting" flag comes from the readiness rows for behaviors bound to that backend; delete `apply_call_state`/`deadline_is_expired` copies in favor of `gents::admission::slot_accounting::{call_state_holds_backend_slot, reconstructed_running_slot_count}` exported `pub` from `gents` plus the deadline helper from #1334 if already merged, else the one in `tool_call_lifecycle`)
- Modify: `crates/gents/src/lib.rs` (re-export the slot accounting functions)
- Test: `crates/gents-cli/src/http/{healthz,fleet_slots}.rs` unit tests updated to build readiness rows instead of raw backend rows; `cargo test -p gents-cli --test cli_server` if it snapshots `/healthz`.

- [ ] Implement; `cargo test -p gents-cli --lib http::` green; grep gate from Global Constraints; commit — `cli: healthz and fleet-slots read the readiness projection (#1332)`.

### Task 3: Gate
- [ ] `cargo test -p gents --lib`, `cargo test -p gents --test conformance backend_health`, `cargo test -p gents-cli`, `cargo check --workspace --all-targets`, `cargo fmt --all --check`; net deletion check. CHANGELOG Unreleased `### Fixed`: "`/healthz` and `gents fleet-slots` now report backends the local prober has vetoed as degraded/not accepting, matching admission."
