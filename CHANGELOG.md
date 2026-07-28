# Changelog

All desktop crates and npm packages release together at `workspace.package.version`
(lockstep train). Bridge **contract** version (`MAJOR.MINOR`) moves independently
and is what compatibility decisions key on — see `contracts/desktop-bridge.json`.

## Unreleased

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
| unreleased | 0.9.0        | 0.9.0        | 0.5              | Reusable desktop packages implemented in #878 |

## 0.8.0

Baseline before reusable package extraction (pre-#877 implementation).
