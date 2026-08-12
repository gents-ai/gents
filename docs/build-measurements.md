# Build measurements

The repository records release and dependency-graph measurements with
`scripts/measure-gents-binary.sh`. Reports use a versioned JSON schema and include
the commit, dirty state, `Cargo.lock` hash, Rust version, target triple, release
profile, build time, binary and archive hashes/sizes, target-scoped dependency
counts, and machine-readable duplicate-version identities.

Use:

```sh
make measure-build-graph
make measure-release-cli
```

Release workflows attach `*.build-metrics.json` beside each published archive.
CI uploads the dependency-graph report as a retained workflow artifact and
renders the same signals in the Actions step summary.

## 2026-08-11 release-profile experiment

Host: Apple M4 Max (16 cores, 128 GB), macOS 26.6, Rust 1.97.1,
`aarch64-apple-darwin`. Source commit: `45673f4d43539060abef2a94599f73e35216e9d4`.
`Cargo.lock` SHA-256:
`adea767d0729635c92ba206200fe86042e33f85057af8e63406799077672214f`.

Each candidate used a target directory without artifacts from another release
profile. The host-local sccache remained enabled, matching normal developer and
release-host operation. The working tree was dirty because it contained the
measurement implementation, but compiled Rust sources were held constant.

| LTO | CGUs | Build | Binary bytes | gzip-9 bytes | Build delta | gzip delta |
|---|---:|---:|---:|---:|---:|---:|
| fat | 1 | 836s | 62,129,152 | 27,774,177 | baseline | baseline |
| thin | 4 | 301s | 73,087,888 | 28,452,730 | -64.0% | +2.4% |
| thin | 16 | 246s | 78,921,424 | 30,091,548 | -70.6% | +8.3% |
| off | 16 | 209s | 70,702,208 | 26,797,220 | -75.0% | -3.5% |

The no-LTO/16-CGU result was reproduced through the ordinary checked-in
`make release-cli` path in 214 seconds with the same 70,702,208-byte binary
size. Binary hashes differ across target roots, so size and executable smoke
checks—not byte identity—are the comparison contract.

Decision: prefer build speed for now. The release and dev-install profiles use
explicit `lto = "off"` and 16 codegen units. This is deliberately `"off"`, not
`false`: Cargo defines `false` as local ThinLTO, while `"off"` disables LTO.
Linux release jobs retain explicit per-architecture job caps for host memory.

The optimized test profile also uses explicit `lto = "off"`. CI's heavy Rust
shards enable incremental compilation over persistent target trees; local
ThinLTO allowed stale cross-codegen-unit imports to survive there and produced
undefined Rust symbols while linking the macOS desktop test binaries. A local
all-targets Tauri test compile with test LTO disabled completed from the newly
invalidated profile in 257 seconds and linked all three test executables.

## Dependency baseline

At the PR #1097 measurement commit, the normal macOS `gents-cli` graph
contained:

- 820 unique package/version entries
- 69 package names resolved at more than one version or source
- 0 upstream `codex-*` packages

The earlier 1,142-package result counted Cargo tree's repeated `(*)` display
rows as distinct entries. The measurement script strips that marker before
deduplicating package/version identities.

### v0.11.0 report reconciliation

The published v0.11.0 reports showed 1,141 macOS packages and 1,129/1,131
Linux packages even though the same lockfile resolves to 819, 813, and 815
packages respectively. This was a measurement defect, not dependency growth
from DefraDB v0.18.0. `dtolnay/rust-toolchain` sets
`CARGO_TERM_COLOR=always` in Actions; Cargo then surrounds only the repeated
`(*)` marker with ANSI escapes, so the script's plain-text suffix removal did
not match. Reproducing the script locally with that environment produced the
published 1,141 count exactly.

Schema version 2 forces color off for Cargo tree data, passes the recorded
target explicitly, records the target-independent `--target all` count, and
includes each duplicated package name with its resolved identities. Current
main (`b254258c`, lockfile SHA-256
`72e775bf1cf70e186f1afa5d0ca314267973b68739995664fa8d0604ad5e1be6`)
measures:

| Target scope | Normal packages |
|---|---:|
| `aarch64-apple-darwin` | 819 |
| `aarch64-unknown-linux-gnu` | 813 |
| `x86_64-unknown-linux-gnu` | 815 |
| all targets | 962 |

Do not compare these target-scoped counts to `cargo tree --target all`, and do
not establish a dependency budget until multiple schema-version-2 reports have
established normal variance.

## v0.11.0 macOS release latency

The tagged release spent 2,005 seconds in Cargo; signing, notarization, dSYM
generation, packaging, and upload together took about two additional minutes.
Its log confirms `SCCACHE_RECACHE=1`, a cold release-only target tree, and a
full compile ending in a roughly ten-minute `gents` compile/link tail. The
209–214 second profile experiment also used a fresh target, but read from the
host's warm shared sccache. Those are intentionally different trust/cache
states: a signed release must not read compiler objects populated by public PR
jobs.

Release Cargo output is already isolated beneath
`CARGO_RELEASE_TARGET_ROOT`, and forced recaching applies only when Cargo
actually invokes rustc. The release workflow is therefore pinned to the
`studio-2-2` (`studio-2`, `ci-desktop`) registration that owns the trusted
release target tree. This makes prior signed-release artifacts reliably warm
without weakening the sccache boundary. Record cold, unchanged-source warm,
and representative source-edit timings separately; the pin trades automatic
runner failover for deterministic trusted-cache reuse.

Two unsigned workflow dispatches at commit `d0e1ca3b` measured the
representative trusted-target rebuild after this change. Cargo rebuilt only
`gents-cli`; all dependencies and the core `gents` runtime remained reusable.
Each clean checkout still invalidated the final crate's compile/link, so these
are not unchanged-source no-op measurements:

| Cache state | Cargo build | Raw binary | gzip -9 | Archive |
|---|---:|---:|---:|---:|
| v0.11.0 cold trusted target | 2,005s | 70,300,272 | 26,580,064 | 26,641,927 |
| pinned trusted target, clean checkout 1 | 524s | 70,299,648 | 26,577,430 | 26,639,171 |
| pinned trusted target, clean checkout 2 | 426s | 70,299,648 | 26,577,430 | 26,639,171 |

The representative clean-checkout builds are 73.9–78.8% faster (3.83–4.71x)
than the cold v0.11.0 release. The dry runs were unsigned, so their size
differences include the missing Developer ID signature and are not a claim of
binary-size improvement. Raw workflows:
https://github.com/source-inc/gents/actions/runs/31601223603 and
https://github.com/source-inc/gents/actions/runs/31602179888.

Useful next investigations are to rank duplicate packages by compiled size,
map features that pull each duplicate version, split optional desktop/provider
surfaces from the runtime-critical CLI graph, and pursue DefraDB feature
boundaries for its internally enabled Wasmtime and libp2p graphs.
