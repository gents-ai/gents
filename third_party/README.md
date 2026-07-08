# Third-party patches

## `noq` / `noq-proto` (1.0.1)

Pinned via `[patch.crates-io]` in the workspace `Cargo.toml` until upstream
releases a fix past crates.io `1.0.1`. These packages are listed under
`workspace.exclude` (each has an empty `[workspace]` table) so they are not
fmt/test targets of the main workspace.

| Upstream | Symptom | Patch |
| --- | --- | --- |
| [n0-computer/noq#743](https://github.com/n0-computer/noq/issues/743) | Duplicate `Draining` endpoint events from a second stateless reset underflows `active_connections` (panic with overflow checks; silent hang on `wait_all_draining` without them) | `noq-proto`: edge-trigger `Draining` on `!was_drained` for the Reset/AEAD arm |
| [n0-computer/noq#723](https://github.com/n0-computer/noq/issues/723) | `EndpointDriver::drop` `unwrap()`s a poisoned mutex → process abort | `noq`: recover with `PoisonError::into_inner` |
| (defense) | Same underflow site if another path reintroduces duplicate Draining | `noq`: per-connection `draining_reported` set before decrementing |
| (defense) | Poisoned Drop abort via `EndpointRef` after driver | `noq`: `EndpointRef::drop` also recovers from poison |

### Why this is in defra-agent

- Production: fleet builds set `overflow-checks = true`, so the underflow kills steward nodes (defradb.rs#1091 / defra-agent#634).
- Conformance: `generated_r5_cross_deployment_cases_drive_production_dispatch` tears down P2P endpoints; a poisoned Drop aborts the whole binary rather than one test.

### Remove when

Upstream cuts a release including #743 (and ideally #723), and `iroh`/`defradb.rs` bump to it. Then delete these crates and the `[patch.crates-io]` entries.

### Regression

```sh
cargo test --manifest-path third_party/noq-proto/Cargo.toml \
  regression_double_stateless_reset_single_draining
```
