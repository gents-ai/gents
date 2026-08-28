# Session Hydration Foundation (#1142) Implementation Plan

**Goal:** Define the tenant-safe, idempotent `SessionHydrationRequest` lifecycle and register its client-authored control collection without inventing a second P2P delivery path.

**Architecture:** Follow the foundation flow: model admission, document selection, terminality, crash re-drive, pairing non-interference, and receiver progress in Lean; fence the executable decisions in Rust; register the branchable schema in the shared catalog, client-authored migration fence, and `machine` template; then run the server sweep through DefraDB's existing peer-targeted document pusher. The client requests hydration on focus and declares completion only after its exact local seven-collection document count reaches the server's selected-document count.

**Spec:** `docs/superpowers/specs/2026-08-17-mobile-session-sync-design.md`

## Review clarifications

- `request_key` remains `"{peer_id}:{session_id}"`; the lifecycle consumes a parsed peer id instead of treating the opaque key as authorization.
- Admission requires a paired peer, requester membership cryptographically verified against the selected network, and an exact `(session_id, requester_did, agent_did)` ownership tuple.
- Selection independently filters every candidate document by the six client-routable transcript collection names, requester, session, and agent. Admission alone never turns an unscoped candidate set into a grant.
- `AgentToolApproval` is selected only through its immutable `tool_call_doc_id` edge to an already-selected `AgentToolCall`; display/request IDs are not authorization keys.
- Re-drive is set-idempotent: a crash after document delivery but before the terminal write can repeat the same selection without widening it.
- Receiver progress is keyed by `(session_id, agent_did)` and resets when focus changes, while remaining monotone for repeated observations of the same target.
- Hydration state does not modify pairing state or template filters.
- Do not implement hydration by global document subscriptions, no-op document rewrites, a manifest collection, or replicator teardown/reinstall.

## Tasks

1. Add `Proofs/SessionHydration/{State,Executable,Properties}.lean` and a barrel import. Prove no push before admission, tenancy/session soundness, idempotent re-drive, terminality, and pairing non-interference with zero `sorry`s.
2. Add a small pure Rust mirror in `p2p_reconcile/session_hydration.rs` and conformance tests for the admission matrix and exact selected document set.
3. Add the branchable `SessionHydrationRequest` SDL to `gents-schemas` and `gents-protocol`; classify it as client-authored so fresh-apply parity remains enforced.
4. Add the collection and requester-scoped rule to the Lean and Rust `machine` catalogs, including exact catalog assertions.
5. Add the cancellable server reconciler, signed-network membership loader, exact approval linkage, bounded peer-targeted delivery, and terminal status writes.
6. Add client focus/request/repair flow with session-scoped progress and exact receiver-side document accounting across all six client-routable collections.
7. Add an ignored two-node live-inference qualification test that creates history before the client exists, installs only the control replication route, and verifies exact replay and receiver completion across all six hydration collections.
8. Run `lake build`, focused schema/template/conformance tests, `cargo test -p gents`, `cargo check --workspace --all-targets`, and the workstation-2 live qualification gate.

## Delivery boundary

The embedded adapter exposes DefraDB's existing `DocPusher`/`TransportDocPusher` path for an explicit document set. Hydration reuses that operation, its persisted replication filters/ACP and replay admission bounds, and applies a bounded admin-call timeout. The reconciler writes `served` only after the exact selected set is accepted; cancellation interrupts a sweep that is waiting on delivery.

## Live qualification

The ignored `live_session_hydration_replays_history_to_a_fresh_client` test uses the existing Rust E2E node/runtime harness. A live GLM turn first creates durable request, response, and message history on the server; deterministic linked witnesses cover tool calls, results, approvals, and compaction. Only then does the test create a fresh client node. The standing server-to-client route carries `SessionHydrationRequest` control rows only, so ordinary transcript replication cannot satisfy the assertion. The production hydration reconciler must explicitly select and push the exact seven-collection document set, return the served count, and drive the shared receiver model to `Complete`.

Run it against workstation-2 with:

```sh
GENTS_LIVE_SESSION_HYDRATION=1 cargo test -p gents --features live-e2e \
  --test e2e_live live_session_hydration_replays_history_to_a_fresh_client \
  -- --ignored --nocapture
```

The test defaults to `http://100.87.27.25:8000/v1` and `GLM-5.2`; `GENTS_LIVE_SESSION_HYDRATION_ENDPOINT` and `GENTS_LIVE_SESSION_HYDRATION_MODEL` override those defaults. This gate covers the protocol/runtime boundary and shared receiver state machine. Desktop start/observe/retry orchestration remains covered by the desktop-core test suite rather than this integration target, avoiding a dependency cycle from `gents` back to `gents-desktop-core`.
