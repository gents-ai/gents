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

### Bridge contract 0.4

- Additive: `Pairing` error code; fingerprint permission inventory aligned with
  grantable `[[set]]` entries (`core`, `client-lifecycle`, bundles).
- `BridgeConfig::default().snapshot_grants` is **fail-closed** `core_only()`.
- Documented v1 process-wide snapshot grant model (not per-caller ACL).

### Bridge contract 0.3

- Additive: structured `BridgeError` on command error paths; `SnapshotGrants`
  projection at the snapshot builder seam; `native-e2e` cargo feature.
- See prior entries for 0.2 (`desktop_bridge_contract`, peer probe by address /
  status by saved peer id).

### Packages

- New npm workspace packages, distributed as GitHub Release tarballs:
  - `@source-inc/gents-desktop-client` — typed transport, shared store, errors, testing
  - `@source-inc/gents-desktop-ui` — accessible shared primitives
  - `@source-inc/gents-desktop-chat` — chat projection, components, and styles
  - `@source-inc/gents-desktop-fleet` — discovery, pairing, health, and peer UI
  - `@source-inc/gents-desktop-operations` — rail, holds, health, lineage, traces
  - `@source-inc/gents-desktop-tokens` — semantic CSS tokens
- Fixture host `apps/fixture-host` consumes all packages, registers a domain
  operations tab, and proves co-residence under separate homes without
  `runtime-admin`.
- Tag releases attach clean-install-verified npm tarballs to the GitHub Release;
  downstreams pin those assets exactly.

### Downstream update workflow

1. Bump the git tag pin for `gents-desktop-bridge` and npm pins to the same `vX.Y.Z`.
2. Read the **Bridge contract** section for additive vs breaking diffs.
3. Run contract + e2e + visual gates (fixture host is the template).
4. Merge.

## Compatibility matrix

| Tag | Bridge crate | npm packages | contract_version | Notes |
|-----|--------------|--------------|------------------|-------|
| unreleased | 0.9.0 | 0.9.0 | 0.5 | Reusable desktop packages implemented in #878 |

## 0.8.0

Baseline before reusable package extraction (pre-#877 implementation).
