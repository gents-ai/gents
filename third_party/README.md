# Third-party patches

## `noq` (1.1.1)

Pinned via `[patch.crates-io]` in the workspace `Cargo.toml` until upstream
resolves [n0-computer/noq#723](https://github.com/n0-computer/noq/issues/723).
The package is listed under `workspace.exclude`, so it is not a fmt/test target
of the main workspace.

Source provenance: `third_party/noq` is the crates.io `1.1.1` source published
from upstream commit
[`12a4bf0b`](https://github.com/n0-computer/noq/commit/12a4bf0b42070b570fb8cf90fe315c630b03f56e).
The table below lists every downstream code deviation.

| Upstream | Symptom | Patch |
| --- | --- | --- |
| [n0-computer/noq#723](https://github.com/n0-computer/noq/issues/723) | `EndpointDriver::drop` `unwrap()`s a poisoned mutex → process abort | `noq`: recover with `PoisonError::into_inner` |
| (defense) | Poisoned Drop abort via `EndpointRef` after driver | `noq`: `EndpointRef::drop` also recovers from poison |

### Why this is in Gents

- Production: noq 1.1.1 now contains the stale-path and duplicate-Draining
  fixes that previously required local `noq-proto` changes.
- Conformance: `generated_r5_cross_deployment_cases_drive_production_dispatch` tears down P2P endpoints; a poisoned Drop aborts the whole binary rather than one test.

### Remove when

Upstream ships #723 and `iroh`/`defradb.rs` resolve that release. Then delete
`third_party/noq` and its `[patch.crates-io]` entry.
