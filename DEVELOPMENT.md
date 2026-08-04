# Development

How to build, test, and work on Gents from source. For installing and
running the prebuilt binary, see the [README](README.md).

## Requirements

The prebuilt binary in [Get running](README.md#get-running) needs a local
inference server (e.g. `llama.cpp`). The `gents codex` command additionally
needs the Codex CLI on `PATH`. Building from source or developing also needs:

- **Rust** — stable toolchain, edition 2021. There is no pinned MSRV; the
  workspace is developed and built against current stable (1.96). Install via
  [rustup](https://rustup.rs); `rustup update stable` keeps you current.
- **C/C++ toolchain** — `cc`/`gcc`, `g++`, and `make`. Several transitive
  dependencies build native code (`aws-lc-sys`, `ring`, and RocksDB).
- **protoc** — the protobuf compiler, required by the `prost-build` step in
  DefraDB's P2P stack (`iroh-bitswap`). Build fails with "Could not find
  `protoc` installation" without it.
  - Debian/Ubuntu: `sudo apt-get install protobuf-compiler`
  - Fedora: `sudo dnf install protobuf-compiler`
  - macOS: `brew install protobuf`
- **libclang** — `librocksdb-sys` runs `bindgen`, which needs the C-API
  `libclang.so` (the C++ `libclang-cpp` is not sufficient). The build fails with
  "Unable to find libclang" without it.
  - Debian/Ubuntu: `sudo apt-get install libclang-dev`
  - Fedora: `sudo dnf install clang-devel`
  - macOS: provided by the Xcode Command Line Tools
- **OpenSSL development headers** — required by the transitive `openssl-sys`
  build (pulled in regardless of CLI features), along with `pkg-config` and
  `perl`. The build fails early without them.
  - Debian/Ubuntu: `sudo apt-get install pkg-config libssl-dev perl`
  - Fedora: `sudo dnf install pkg-config openssl-devel perl`
  - macOS: `brew install openssl pkg-config` (Xcode Command Line Tools supply
    the C toolchain)
- **Git + HTTPS access to GitHub** — the workspace pins public `defradb.rs` and
  `backbone` revisions. No GitHub SSH key or private-repository token is needed.
- **Lean toolchain** (proofs only) — [`elan`](https://github.com/leanprover/elan)
  provides `lake`/`lean` for `crates/gents/proofs`.
- **Desktop app** (`apps/gents-desktop`, optional) — Node.js with `npm`, and
  the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).
  On Debian/Ubuntu that means the GTK/WebKit development libraries:
  `sudo apt-get install libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`.
  A plain `cargo build` includes this crate (it is a workspace default member);
  to build just the runtime and CLI without the GTK toolchain, use
  `cargo build --workspace --exclude gents-desktop-tauri`.

A fast linker is wired up automatically when present: the workspace
`.cargo/config.toml` routes Linux and macOS builds through `mold`/`lld` (falling
back to the toolchain's bundled `rust-lld`, then the system linker), which cuts
link time on incremental rebuilds.

## Building and testing

```bash
make help                                    # curated build/test targets
cargo test -p gents                         # runtime suite (lib + integration)
cargo test --workspace                       # everything
cargo build -p gents-cli                     # CLI + Codex app-server shim
cd crates/gents/proofs && lake build        # the Lean proofs
```

The development flow is foundation-first: Lean model → conformance tests → implementation. `CLAUDE.md` is the working brief; the [proofs README](crates/gents/proofs/README.md) maps the formal coverage.

## Binary size and compile graph

Use the checked-in measurement script so comparisons use the shipped release
profile and the same package-counting rules:

```bash
scripts/measure-gents-binary.sh
```

It reports release-build wall time, on-disk binary size, native section sizes,
unique resolved packages, and Codex packages in the CLI graph. The default
timing is incremental. For an isolated cold build, point `CARGO_TARGET_DIR` at
an empty directory; set `SKIP_BUILD=1` to inspect an existing release binary
without rebuilding.

```bash
CARGO_TARGET_DIR=/tmp/gents-release-measure scripts/measure-gents-binary.sh
```

For symbol- and crate-level attribution, install
[`cargo-bloat`](https://github.com/RazrFalcon/cargo-bloat) and run:

```bash
cargo bloat --release --locked -p gents-cli --bin gents --crates
cargo bloat --release --locked -p gents-cli --bin gents -n 40
```

For iteration, the `dev-install` profile keeps release optimization but uses
thin LTO and parallel codegen. Release-size comparisons must use `release`,
which retains fat LTO and one codegen unit.
