# Changelog

All desktop crates and npm packages release together at `workspace.package.version`
(lockstep train). Bridge **contract** version (`MAJOR.MINOR`) moves independently
and is what compatibility decisions key on — see `contracts/desktop-bridge.json`.

## Unreleased

## 0.11.0 - 2026-08-11

### Bridge contract

- Advance the additive desktop bridge contract from 0.5 to 0.9: Grok OAuth
  login, managed local-server lifecycle and tray events, and provider-account
  inventory/disconnect controls (#973, #1013, #1089).
- Advance the Rust and npm desktop package train together to 0.11.0.

### Runtime and formal foundation

- Make schema provenance and signing DefraDB-native, and persist the exact
  rendered provider request before send (#1087, #1059).
- Add deterministic inference seeds and request-wide token budgets (#1062).
- Extend the Lean-fenced runtime foundation across prompt assembly, compaction,
  admission slot accounting, tool-call CAS transitions, recovery, and
  background/subagent lifecycle convergence (#999, #998, #1007, #1006).
- Align durable tool outcomes across timeline projections and add
  Harbor-compatible ATIF trace export (#1095, #1098, #988).

### Agents, configuration, and automation

- Add persona request flows, reusable directory persona and inference-profile
  catalogs, and safer self-configuration ergonomics (#1028, #1014, #1050,
  #1052, #1056, #1057).
- Add document-driven EventTrigger graph experiments and capture consumers
  for persisted runtime facts (#1081, #1080).

### Providers, CLI, and desktop

- Add Grok/xAI subscription OAuth across the runtime, CLI, and desktop (#973,
  #974), plus provider-account settings and disconnect controls (#1089).
- Make local desktop agent onboarding optional and add managed-server controls
  (#1013).
- Align CLI initialization and lineage behavior with signed provenance, while
  retaining compatibility with older provenance JSON (#1094, #1092).

### Build and reliability

- Split and cache Rust CI workloads, slim the CLI dependency graph, and add
  release dependency/binary metrics (#1011, #1024, #1097).
- Favor faster local and release builds with non-LTO profiles and parallel code
  generation; harden runtime cancellation, bridge reconciliation, and
  resource-contended conformance startup (#1097).

### Dependencies

- Advance DefraDB to v0.18.0 (`61e429fc`).

## 0.10.1 - 2026-07-30

### Dependencies

- Advance DefraDB to v0.17.4 (`f9e21c68`), following the v0.17.3 pin from #972.

### Runtime and reliability

- Stabilize mobile chat and stop synthetic agent turns (#930).
- Complete native backgrounding for subagents and tools with real GLM E2E (#937, #945).
- Fix machine pairing replicator install: `source_did` must be `@immutable` (#939).
- Hard-fail materialize + restore post-migration CI gates; complete and pin the
  runtime migration baseline catalog (#947, #949).

### Cleanup

- Integration cleanup pass for runtime, desktop, CLI, and tests (#970, #971).
- Clippy and harness hygiene for trigger tests (#961).

## 0.10.0 - 2026-07-29

### Bridge contract

- 0.5 (additive): merged #871 inference onboarding —
  `desktop_probe_inference_endpoint`, `desktop_codex_login`, and
  `desktop_codex_login_cancel` under `config-write`; new one-shot
  `desktop://codex-login-url` event.
- Inference onboarding request, response, and login-URL event payloads now come
  from generated Rust bindings instead of handwritten TypeScript mirrors.
- Local runtime initialization/reset is serialized with client start/shutdown
  and rejects storage mutation while a client is live.

### Runtime reliability

- Request interrupt latches use the standard bounded DefraDB
  transaction-conflict retry, eliminating an observed cascade-conformance flake.

### Bridge contract 0.4

- Additive: `Pairing` error code; fingerprint permission inventory aligned with
  grantable `[[set]]` entries (`core`, `client-lifecycle`, bundles).
- `BridgeConfig::default().snapshot_grants` is **fail-closed** `core_only()`.
- Documented v1 process-wide snapshot grant model (not per-caller ACL).

### Bridge contract 0.3

- Additive: structured `BridgeError` on command error paths; `SnapshotGrants`
  projection at the snapshot builder seam; `native-e2e` cargo feature.
- Command failures now serialize as `{ code, message, retryable }`; the client
  accepts both structured errors and legacy bare strings during migration.
- `RenderedTimelineItem` variant fields now serialize in camelCase, repairing
  the latent Rust/frontend mismatch (`itemKey`, not `item_key`).
- Breaking for pre-package bridge consumers: `desktop_peer_status_fetch`
  accepts a saved `peerId`, not an arbitrary `serverAddress`; arbitrary-address
  probing is restricted to the fleet-admin command.
- See prior entries for 0.2 (`desktop_bridge_contract`, address probe, and the
  saved-peer status lookup).

### Packages

- New npm workspace packages, distributed as GitHub Release tarballs:
  - `@source-inc/gents-desktop-client` — typed transport, shared store, errors, testing
  - `@source-inc/gents-desktop-ui` — accessible shared primitives
  - `@source-inc/gents-desktop-chat` — chat projection, components, and styles
  - `@source-inc/gents-desktop-fleet` — discovery, pairing, health, and peer UI
  - `@source-inc/gents-desktop-operations` — rail, holds, health, lineage, traces
  - `@source-inc/gents-desktop-tokens` — semantic CSS tokens
- Fixture host `apps/fixture-host` consumes all packages, renders bridge session
  snapshots, and registers a file-backed domain operations tab without
  `runtime-admin`. It proves package/plugin composition; the automated two-node
  Amygdala journey remains downstream evidence.
- Tag releases attach clean-install-verified npm tarballs to the GitHub Release;
  downstreams pin those assets exactly.

### Downstream update workflow

1. Bump the git tag pin for `gents-desktop-bridge` and npm pins to the same `vX.Y.Z`.
2. Read the **Bridge contract** section for additive vs breaking diffs.
3. Run contract + e2e + visual gates (fixture host is the template).
4. Merge.

## Compatibility matrix

| Tag        | Bridge crate | npm packages | contract_version | Notes                                         |
| ---------- | ------------ | ------------ | ---------------- | --------------------------------------------- |
| v0.11.0    | 0.11.0       | 0.11.0       | 0.9              | DefraDB v0.18.0; signed provenance and build metrics |
| v0.10.1    | 0.10.1       | 0.10.1       | 0.5              | DefraDB v0.17.4; mobile/subagent/migration fixes |
| v0.10.0    | 0.10.0       | 0.10.0       | 0.5              | Reusable desktop packages implemented in #878 |

## 0.8.0

Baseline before reusable package extraction (pre-#877 implementation).
