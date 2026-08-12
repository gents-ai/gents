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
make measure-build-attribution
```

Release workflows attach `*.build-metrics.json` beside each published archive.
CI uploads the dependency-graph report as a retained workflow artifact and
renders the same signals in the Actions step summary.

The manual Build attribution workflow uses a fresh target directory with no
compiler cache and uploads raw Cargo timings, resolved metadata and feature
tree, duplicate-version paths, and `cargo-bloat` 0.12.1 linked-size rankings.
Its `summary.json` keeps build time, peak memory, binary size, dependency count,
aggregate per-package compile duration, and linked contribution as separate
signals. The raw reports are retained as workflow artifacts rather than checked
into the repository.

For local agent and batch worktrees, `make fast-dev-cli` uses the opt-in
`fast-dev` profile. It retains line-table backtraces for workspace crates while
omitting dependency debug info. The ordinary dev profile remains unchanged for
debugger-heavy work. Do not compare `fast-dev` artifacts to release binaries;
the profile addresses edit/build time and retained worktree disk space only.

### Opt-in fast dev profile

Host: Apple M4 Max (16 cores, 128 GB), macOS 26.6, Rust 1.97.1,
`aarch64-apple-darwin`. Base commit: `69acf8b1`. `Cargo.lock` SHA-256:
`72e775bf1cf70e186f1afa5d0ca314267973b68739995664fa8d0604ad5e1be6`.

Both builds used all available CPUs, fresh target directories, direct rustc
without sccache, and `CARGO_INCREMENTAL=0`. Rust sources and the lockfile were
identical; only the candidate profile settings differed. Commands were:

```sh
CARGO_BUILD_RUSTC_WRAPPER=.github/scripts/rustc-direct.sh \
  RUSTC_WRAPPER=.github/scripts/rustc-direct.sh CARGO_INCREMENTAL=0 \
  /usr/bin/time -l cargo build -p gents-cli --locked --target-dir <fresh-target>
CARGO_BUILD_RUSTC_WRAPPER=.github/scripts/rustc-direct.sh \
  RUSTC_WRAPPER=.github/scripts/rustc-direct.sh CARGO_INCREMENTAL=0 \
  /usr/bin/time -l cargo build -p gents-cli --profile fast-dev --locked \
  --target-dir <fresh-target>
```

| Signal | Default dev | `fast-dev` | Delta |
|---|---:|---:|---:|
| Cold wall time | 212.10s | 177.11s | -16.5% |
| Peak RSS | 4,506,910,720 | 3,967,090,688 | -12.0% |
| Target size (KiB) | 11,669,916 | 5,144,180 | -55.9% |
| `deps/` size (KiB) | 8,694,172 | 4,041,368 | -53.5% |
| rlib size (KiB) | 4,546,124 | 2,006,892 | -55.9% |
| loose object size (KiB) | 2,638,220 | 558,200 | -78.8% |
| Unstripped dev CLI bytes | 328,694,312 | 312,358,584 | -5.0% |

An identical one-line `gents-cli` source edit rebuilt and linked in 1.58s under
default dev and 1.34s under `fast-dev` (-15.2%). These are dev artifacts, not
release-size measurements. The default dev profile remains the debugger-rich
escape hatch; `fast-dev` trades dependency DWARF for materially smaller agent
worktrees while retaining workspace file-and-line backtraces.

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

### Same-commit release retries

Cargo build-script fallback discovery watches the checkout's Git `HEAD` and
resolved ref so local builds report accurate provenance. A release checkout is
new on every workflow invocation, however, and those Git files receive new
timestamps even when the source commit is unchanged. That invalidated
`gents-cli` and its final link on every retry.

Release workflows now pass the trusted GitHub event SHA, ref, tag, and clean
state to the build script explicitly. The local discovery fallback is
unchanged, while an explicit-metadata release no longer watches checkout-local
Git files. Two unsigned dispatches of commit `ef1585af` on the same trusted
target tree measured the creation and reuse of that artifact:

| Cache state | Cargo build | Workflow job | Raw binary | gzip -9 | Archive |
|---|---:|---:|---:|---:|---:|
| first explicit-metadata artifact | 424s | 8m32s | 70,300,112 | 26,575,189 | 26,637,333 |
| identical-SHA retry | 0.53s | 1m34s | 70,300,112 | 26,575,189 | 26,637,341 |

The retry avoids 423 seconds (99.9%) of Cargo work and cuts the complete job by
6m58s (81.6%). The eight-byte archive difference is packaging metadata; the raw
binary and gzip payload sizes are identical. This optimization affects only
exact same-commit retries. A source edit still requires the representative
final-crate compile/link measured above. `SCCACHE_RECACHE=1` remains enabled,
and no public-PR compiler artifacts enter the trusted release target. Raw
workflows: https://github.com/source-inc/gents/actions/runs/31608891843 and
https://github.com/source-inc/gents/actions/runs/31609716279.

### Cold compile and linked-size attribution

A manual attribution run at commit `ac11b9e0` used Rust 1.97.1, a fresh
`aarch64-apple-darwin` target directory, direct rustc with no compiler cache,
12 Cargo jobs, and `CARGO_INCREMENTAL=0`. The ordinary stripped release build
took 1,788 seconds (29m48s) and peaked at 10,174,103,552 bytes RSS. It produced
a 70,693,328-byte binary and 26,994,237-byte gzip payload with 819 target-scoped
normal packages, 962 all-target normal packages, and 68 duplicate package
names.

This puts the v0.11.0 2,005-second release and the new 1,788-second cold build
in the same regime. It also isolates the main reason they differ from the
209--214-second profile experiment: the latter could read the host's warm
shared compiler cache, whereas both cold measurements had to compile the
native dependency graph. Signing and packaging are outside the 1,788-second
Cargo measurement. The same-commit trusted-target retry above remains the
representative warm/no-op result.

Cargo timing durations are aggregated across every unit for a package and can
overlap in wall time. The largest cold contributors were:

| Package | Aggregate compile duration | Longest unit |
|---|---:|---:|
| `librocksdb-sys` | 519.58s | 494.09s |
| `gents-cli` | 405.37s | 345.88s |
| `aws-lc-sys` | 293.14s | 290.62s |
| `cranelift-codegen` | 189.13s | 176.54s |
| `gents` | 178.74s | 178.74s |
| `iroh` | 158.03s | 131.48s |
| `gents-migration` | 156.83s | 139.64s |
| `rmcp` | 121.55s | 120.49s |
| `wasmtime-internal-core` | 115.40s | 57.45s |
| DefraDB `p2p` | 97.52s | 97.52s |

`cargo-bloat` 0.12.1 then performed a separate 18m01s symbol-preserving
rebuild. Its 122,703,208-byte inspection binary is not the shipped binary; the
57,632,052-byte text section is useful only for linked-code attribution. The
largest linked contributors were `gents_server` (7,106,696 bytes), `std`
(6,201,385), `gents` (5,701,092), `librocksdb_sys` (3,189,724), DefraDB `db`
(2,590,420), DefraDB `query` (2,155,308), `cranelift_codegen` (2,107,580), and
DefraDB `p2p` (1,881,492).

The cold timing and linked rankings independently point at the same upstream
boundaries: SourceHub/AWS-LC, Wasmtime/Cranelift, and the combined Iroh plus
legacy-libp2p graph. Those are tracked in DefraDB issues
[1398](https://github.com/sourcenetwork/defradb.rs/issues/1398),
[1400](https://github.com/sourcenetwork/defradb.rs/issues/1400), and
[1399](https://github.com/sourcenetwork/defradb.rs/issues/1399) respectively.
RocksDB remains the largest native cold-build contributor, but it is part of
Gents' required runtime storage path rather than an optional surface identified
for removal.

Exact commands and tool versions are stored with the raw reports from
https://github.com/source-inc/gents/actions/runs/31613287268. The full manual
job took 51m29s because it intentionally measured both the stripped cold build
and the separate symbol build; do not use that job duration as the release
build time.

Useful next investigations are to rank duplicate packages by compiled size,
map features that pull each duplicate version, split optional desktop/provider
surfaces from the runtime-critical CLI graph, and pursue DefraDB feature
boundaries for its internally enabled Wasmtime and libp2p graphs.

### Lark default-storage follow-up

A directly comparable run at commit `28269d4f` replaced RocksDB with Lark as
Gents' default persistent runtime backend. It used the same Rust version,
target, release profile, 12 Cargo jobs, fresh target directory, direct rustc,
disabled incremental compilation, and attribution script as the `ac11b9e0`
run above. The measurement commit differs from the final implementation only
by the temporary workflow-dispatch bridge required before the reusable
attribution workflow reaches the default branch.

| Metric | RocksDB parent | Lark candidate | Change |
|---|---:|---:|---:|
| cold release build | 1,788s | 874s | -914s (-51.1%) |
| stripped binary | 70,693,328 | 64,626,896 | -6,066,432 (-8.6%) |
| gzip payload | 26,994,237 | 24,462,339 | -2,531,898 (-9.4%) |
| target-scoped normal packages | 819 | 819 | 0 |
| all-target normal packages | 962 | 962 | 0 |
| duplicate package names | 68 | 68 | 0 |

The 874-second (14m34s) build peaked at 10,147,430,400 bytes RSS. The package
counts stay flat because small pure-Rust Lark packages replace the native
RocksDB subtree; package count is not a proxy for the measured compile-time or
linked-size improvement. `librocksdb-sys`, `rocksdb`, `bindgen`, and their
native compression/sys dependencies are absent from the candidate normal
graph.

The separate symbol-preserving inspection build produced a 113,352,008-byte
file with a 52,619,664-byte text section, down from 122,703,208 and 57,632,052
bytes respectively. Its largest remaining linked crates are `gents_server`
(7,058,176 bytes), `std` (6,890,816), `gents` (5,694,352), DefraDB `db`
(2,592,192), DefraDB `query` (2,155,884), `cranelift_codegen` (2,106,924), and
DefraDB `p2p` (1,880,936). The remaining cold-build leaders are `aws-lc-sys`
(292.95s aggregate), `gents-cli` (222.28s), and `cranelift-codegen` (212.56s),
which leaves the upstream DefraDB feature boundaries as the next large target.

The complete attribution job took 28m48s because it includes both the ordinary
stripped build and the separate symbol build; only the embedded 874-second
Cargo measurement is comparable with release build time. Exact commands, tool
versions, timings, dependency trees, and linked-size reports are stored in
https://github.com/source-inc/gents/actions/runs/31622045427.
