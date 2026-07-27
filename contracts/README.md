# Bridge contracts

Versioned, committed snapshots that fence the desktop bridge ↔ TypeScript boundary.

| Artifact | Owner | Freshness gate |
|---|---|---|
| `desktop-bridge.json` | `gents-desktop-bridge::contract` | `cargo test -p gents-desktop-bridge fingerprint_matches` |
| `crates/gents-desktop-bridge/bindings/*.ts` | `ts-rs` derives on bridge types | `cargo test -p gents-desktop-bridge committed_bindings` |

## Regenerating

```bash
cargo test -p gents-desktop-bridge write_fingerprint -- --ignored
cargo test -p gents-desktop-bridge write_bindings -- --ignored
```

## Contract versioning

`contract_version` is `MAJOR.MINOR`:

- **MINOR** — additive (new commands, optional fields, event reasons, error codes)
- **MAJOR** — breaking (removal, rename, shape/meaning change)

Any fingerprint diff must bump the version to match the classification in the same PR.

## Type generation

Phase-2 spike chose **ts-rs** over typeshare (serde-compat for `rename_all` and
tagged enums; nested export). Bindings currently cover the error taxonomy and a
representative view-model set; full coverage lands with the phase-3 pluginization
window. Generated files move into `@source-inc/gents-desktop-client/src/generated/`
in phase 5.
