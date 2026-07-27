# Changelog

All desktop crates and npm packages release together at `workspace.package.version`
(lockstep train). Bridge **contract** version (`MAJOR.MINOR`) moves independently
and is what compatibility decisions key on — see `contracts/desktop-bridge.json`.

## Unreleased

### Bridge contract 0.3

- Additive: structured `BridgeError` on command error paths; `SnapshotGrants`
  projection at the snapshot builder seam; `native-e2e` cargo feature.
- See prior entries for 0.2 (`desktop_bridge_contract`, peer probe by address /
  status by saved peer id).

### Packages

- New npm workspace packages (GitHub Packages / release tarballs pending phase-10
  dry-run):
  - `@source-inc/gents-desktop-client` — transport, store, errors, testing
  - `@source-inc/gents-desktop-chat` — headless chat projection, `useMasterDetail`
  - `@source-inc/gents-desktop-fleet` — fleet metrics, peer errors
  - `@source-inc/gents-desktop-operations` — operations rail registry
  - `@source-inc/gents-desktop-tokens` — semantic CSS tokens
- Fixture host `apps/fixture-host` + `fixture-domain-plugin` prove co-residence
  under separate homes (phase 4).

### Downstream update workflow

1. Bump the git tag pin for `gents-desktop-bridge` and npm pins to the same `vX.Y.Z`.
2. Read the **Bridge contract** section for additive vs breaking diffs.
3. Run contract + e2e + visual gates (fixture host is the template).
4. Merge.

## Compatibility matrix

| Tag | Bridge crate | npm packages | contract_version | Notes |
|-----|--------------|--------------|------------------|-------|
| unreleased | 0.8.0 | 0.8.0 | 0.3 | Package extraction in progress on #878 |

## 0.8.0

Baseline before reusable package extraction (pre-#877 implementation).
