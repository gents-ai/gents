# Client synchronization ownership

**Status:** canonical after the September 2026 hard cutover. This file replaces
the earlier mobile-session-sync design so links from historical plans resolve
to current architecture rather than the discarded implementation.

## Ownership chain

There is one path from database facts to product status:

1. DefraDB owns replication work, retry scheduling, exhaustion, quarantine,
   backlog, and coordinator state.
2. `gents::p2p_observability::JsonP2pSyncStatusAdapter` strictly decodes the
   complete DefraDB status. Missing or unknown fields are errors; there are no
   version aliases or defaulted fields.
3. `gents_desktop_core::client::core::sync_state::ClientSyncStateOwner` is the
   sole in-process owner. One coherent revision contains DefraDB status,
   transport health, configured peers, and per-peer route/pairing facts.
4. `gents_desktop_core::client::sync_projection::project_sync_health` is the
   only Rust product projection. Its vocabulary is `healthy`, `syncing`,
   `offline`, and `failed`.
5. The desktop bridge serializes that result as required-but-nullable
   `syncHealth`. It does not derive another answer.
6. `packages/gents-desktop-client/src/operationalState.ts` owns product copy and
   actions. Every app surface consumes that projector.

`AgentBehaviorReadiness` has a different job: the runtime publishes process,
generation, binding, and admission readiness there. It must never be joined
with client sync observations to manufacture transport or replication health.
Session hydration remains its own document lifecycle and progress projection;
it may explain selected-session loading without becoming global sync state.

## Fail-closed rules

- A DefraDB status read or decode error is `failed`; it never degrades the
  transport probe and never masquerades as active synchronization.
- No database observation is `null`/not-yet-observed, not invented health.
- Offline is determined from the configured peers in the same owner revision,
  not unrelated global connection counts.
- Quarantined work is failed. Pending work and retry backlog are syncing.
- Pairing and route retry state stays per deployment and cannot create a global
  database-sync lifecycle.
- A delayed projection cannot overwrite a newer owner revision.

## Removed concepts

Do not restore any of these, including as compatibility behavior:

- synthetic `stalled` sync state or wall-clock/stuck-since inference;
- a second desktop sync DTO, owner, command, polling loop, or projector;
- status inferred from peer labels, pairing retries, hydration, runtime
  readiness, or accumulated UI state;
- optional/omitted `syncHealth` wire fields or serde defaults that accept an
  older shape;
- agent-DID compatibility joins or source-less peer authority;
- UI-local interpretations of raw DefraDB JSON.

Historical issue descriptions and performance scenario names are evidence of
what was measured, not permission to reintroduce those concepts. Change this
contract first if ownership genuinely changes, then update its conformance and
projection tests before implementation.
