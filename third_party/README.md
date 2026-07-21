# Third-party patches

## `noq` / `noq-proto` (1.0.1)

Pinned via `[patch.crates-io]` in the workspace `Cargo.toml` until upstream
releases a fix past crates.io `1.0.1`. These packages are listed under
`workspace.exclude` (each has an empty `[workspace]` table) so they are not
fmt/test targets of the main workspace.

Source provenance: both directories are the crates.io `1.0.1` sources. The
table below lists every downstream code deviation. The #732 change is copied
from upstream merge commit
[`52a50004`](https://github.com/n0-computer/noq/commit/52a500044d47ade2ebba07e0bb59c7b224104df5).

| Upstream | Symptom | Patch |
| --- | --- | --- |
| [n0-computer/noq#732](https://github.com/n0-computer/noq/pull/732) | A stale coalesced datagram routed after its path state is discarded reaches `path_data_mut(path_id).expect("known path")` and panics | `noq-proto`: early-discard packets absent from `paths` but present in `abandoned_paths`; includes the upstream regression test |
| [n0-computer/noq#743](https://github.com/n0-computer/noq/issues/743) | Duplicate `Draining` endpoint events from a second stateless reset underflows `active_connections` (panic with overflow checks; silent hang on `wait_all_draining` without them) | `noq-proto`: edge-trigger `Draining` on `!was_drained` for the Reset/AEAD arm |
| [n0-computer/noq#723](https://github.com/n0-computer/noq/issues/723) | `EndpointDriver::drop` `unwrap()`s a poisoned mutex → process abort | `noq`: recover with `PoisonError::into_inner` |
| (defense) | Same underflow site if another path reintroduces duplicate Draining | `noq`: per-connection `draining_reported` (kept past `Drained`); ignore Draining when handle not in `senders` |
| (defense) | Poisoned Drop abort via `EndpointRef` after driver | `noq`: `EndpointRef::drop` also recovers from poison |

### Why this is in gents

- Production: fleet builds set `overflow-checks = true`, so the underflow kills steward nodes (defradb.rs#1091 / gents#634).
- Production: stale packets for a recently abandoned path panic hub nodes during
  multipath churn (defradb.rs#1090).
- Conformance: `generated_r5_cross_deployment_cases_drive_production_dispatch` tears down P2P endpoints; a poisoned Drop aborts the whole binary rather than one test.

### Remove when

Upstream cuts a release including #732 and #743 (and ideally #723), and
`iroh`/`defradb.rs` bump to it. Then delete these crates and the
`[patch.crates-io]` entries. If a release omits #723, retain or rebase the two
poison-recovery deviations explicitly rather than dropping them silently.

### Regression

```sh
cargo test --manifest-path third_party/noq-proto/Cargo.toml \
  stale_coalesced_datagram_after_path_discard_is_ignored
cargo test --manifest-path third_party/noq-proto/Cargo.toml \
  regression_double_stateless_reset_single_draining
```
