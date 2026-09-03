# #1343 One Rule for Request State in External Projections Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** External adapter projections name request state one way.

**Ruling (controller):** Keep the target framework's own field name, drop the gents-specific duplicate. That is what LangGraph state history already does after #1330 and what the `AmyToolCallTraceRecord.request_status` decision implied (an external contract keeps its field; we do not also emit `lifecycle_state`). Where a framework has no state field of its own, `lifecycle_state` is the one name.

**Tech Stack:** Rust.

**Spec:** GitHub issue #1343. Depends on #1330.

## Global Constraints

- Each projection emits exactly one request-state field; JSON schemas in `adapter_projection.rs` (`~1337`) and any fixture files under `crates/gents/tests` or `crates/gents-cli/tests` (adapter interop round-trips) are updated together with the emitters.
- `toolset/session_history.rs:~928` stops folding `AgentSession.status` and request `lifecycle_state` into one `status` string: two fields (`session_status`, `latest_request_lifecycle_state`) or one vocabulary; pick two fields.
- Net code deletion.

---

### Task 1: OpenAI-Codex and ATIF projections
- [ ] `crates/gents/src/adapter_projection.rs:~1863` and `adapter_projection/atif.rs:~148, ~331`: drop the `lifecycle_state` duplicate where the framework field `status` carries the same value (or the reverse if the framework has no `status`); update the JSON schema at `~1337`; update round-trip tests (`crates/gents-cli/tests/suites/cli_adapter_interop_roundtrip.rs`, `adapter_projection/tests.rs`).
- [ ] `cargo test -p gents --lib adapter_projection`, `cargo test -p gents-cli --test suites cli_adapter_interop_roundtrip`; commit — `projection: one request-state field per external framework (#1343)`.

### Task 2: session_history mixed vocabulary
- [ ] `crates/gents/src/toolset/session_history.rs:~928` `SessionHistoryRow.status`: split into two typed fields; update the tool's JSON output tests.
- [ ] Commit — `toolset: session history separates session status from request state (#1343)`.

### Task 3: Gate
- [ ] `cargo test -p gents`, `cargo test -p gents-cli`, `cargo check --workspace --all-targets`, `cargo fmt --all --check`; CHANGELOG `### Breaking changes` line naming the removed duplicate fields per projection.
