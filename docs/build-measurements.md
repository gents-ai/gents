# Build measurements

The repository records release and dependency-graph measurements with
`scripts/measure-gents-binary.sh`. Reports use a stable JSON schema and include
the commit, dirty state, `Cargo.lock` hash, Rust version, target triple, release
profile, build time, binary and archive hashes/sizes, and dependency counts.

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

The normal `gents-cli` graph currently contains:

- 820 unique package/version entries
- 69 package names resolved at more than one version or source
- 0 upstream `codex-*` packages

The earlier 1,142-package result counted Cargo tree's repeated `(*)` display
rows as distinct entries. The measurement script strips that marker before
deduplicating package/version identities.

Useful next investigations are to rank duplicate packages by compiled size,
map features that pull each duplicate version, split optional desktop/provider
surfaces from the runtime-critical CLI graph, and add explicit regression
budgets only after several release reports establish normal variance.
