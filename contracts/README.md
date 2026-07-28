# Bridge contracts

Versioned, committed snapshots that fence the desktop bridge ↔ TypeScript boundary.

| Artifact                                           | Owner                            | Freshness gate                                           |
| -------------------------------------------------- | -------------------------------- | -------------------------------------------------------- |
| `desktop-bridge.json`                              | `gents-desktop-bridge::contract` | `cargo test -p gents-desktop-bridge fingerprint_matches` |
| `packages/gents-desktop-client/src/generated/*.ts` | `ts-rs` derives on bridge types  | `cargo test -p gents-desktop-bridge committed_bindings`  |

The bridge suite also cross-checks the fingerprint command inventory against
the plugin `generate_handler!` registration, `build.rs` command list, and both
permission TOML files, including the exact expanded production `full` bundle.
Bridge-visible serialized/deserialized Rust contract types must derive `TS`, and
every derived type — including inference command and event payloads — must appear
in the regenerated binding set.

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

Phase-2 selected **ts-rs** (serde-compat for `rename_all` and tagged enums;
nested export). Bindings cover the complete public bridge request/view surface and
are emitted to `@source-inc/gents-desktop-client/src/generated/`. The client
package's public aliases consume those generated files directly.
