# Development

Building Gents from source. To use Gents, install the desktop app from the
[README](README.md) instead.

## Requirements

- **Rust** — via [rustup](https://rustup.rs), which installs the toolchain
  pinned in `rust-toolchain.toml`.
- **C/C++ toolchain** — `cc`, `c++`, and `make` (native crypto dependencies).
- **protoc** — `brew install protobuf` or `apt-get install protobuf-compiler`.
- **OpenSSL headers, `pkg-config`, and `perl`** — `brew install openssl pkg-config`
  or `apt-get install pkg-config libssl-dev perl`.
- **Git with HTTPS access to GitHub** — DefraDB dependencies are public, pinned
  revisions.
- **Lean** (proofs only) — [`elan`](https://github.com/leanprover/elan).
- **Desktop app** (optional) — Node.js 22+ with `npm`, and the
  [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/). On
  Debian/Ubuntu: `apt-get install libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev`.
  To build without the GTK toolchain, use
  `cargo build --workspace --exclude gents-desktop-tauri`.

## Build and test

```bash
make help                                    # curated build/test targets
cargo test -p gents                          # runtime suite
cargo check --workspace --all-targets
cargo test --workspace                       # everything
cd crates/gents/proofs && lake build         # Lean proofs
```

Desktop app, from the repository root:

```bash
npm ci                                       # npm workspaces
make desktop-native-dev                      # run the Tauri app
npm --prefix apps/gents-desktop run test:ui  # deterministic UI gate
make desktop-native-build                    # distributable bundle
```

Use `make desktop-native-build` rather than a raw `tauri build`: it builds and
stages the target-suffixed CLI sidecar that the bundle's `externalBin` requires.
Set `GENTS_DESKTOP_HOME` to an empty directory to exercise first-run setup
without touching your own agent.

`AGENTS.md` holds the working rules for changes; the
[proof map](crates/gents/proofs/README.md) identifies the modeled surfaces.
