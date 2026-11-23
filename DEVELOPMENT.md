# Development

Building Gents from source. To use Gents, install the desktop app as
described at [docs.gents.xyz](https://docs.gents.xyz) instead.

## Requirements

- **Rust** — via [rustup](https://rustup.rs), which installs the toolchain
  pinned in `rust-toolchain.toml`.
- **C/C++ toolchain** — `cc`, `c++`, and `make` (native crypto dependencies).
- **CMake** — `brew install cmake` or `apt-get install cmake`.
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
make test-agent                              # runtime crates (the CI runtime suite)
make test-eval-drivers                       # eval and optimization drivers (gents-eval)
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
Launch the app with `--desktop-home` naming an empty directory to exercise first-run setup
without touching your own node.

`AGENTS.md` holds the working rules for changes; the
[proof map](crates/gents/proofs/AGENTS.md) identifies the modeled surfaces.

## Start a Vera home with device enrollment

Build the CLI with `cargo build -p gents-cli --features vera`. For a new home,
pass `--document-acp vera --create-transport-group` to `gents init` alongside
your usual model and identity options. The client signs the creation of its
device group and the admission of this runtime. The JSON result includes
`transport_group.account` (the client DID) and `transport_group.group` (the
document ID for later membership changes).

Start the same home with `gents server --transport-account <client-DID>`.
The server checks the account's certified assignment before opening a listener.
Registry, endpoint, directory and enrollment operations check membership again
when they run. Revoking the runtime's membership stops subsequent publication.
A Vera home started without the account option leaves peer publication idle.

By default a Vera home runs in process with its Node as the sole operator.
To share a deployment, initialize the host with `--document-acp vera
--vera-rpc-port 9841`. Its account and Node are the initial operators, and
administrative changes require both signatures. Initialization creates a device group owned by the account and admits the host. Save the `vera_anchor` object
from the initialization result and give it to the second device over an
authenticated channel. It identifies the account, deployment, consensus key and genesis.

Start the host before initializing the follower. Use the host account to add
the follower device’s DID to the group’s `member` relation with
`gents runtime p2p enrollment admit-device <group-id> <device-did>`.
Use `remove-device` with the same arguments to withdraw it. The anchor identifies
the deployment, while that signed grant admits the device. On the second device, make
a private route from a local loopback port to the host's loopback port.
Initialize a fresh follower with `--document-acp vera --vera-rpc-port 9841
--vera-anchor host-anchor.json`, using the local end of that route as the
port. The follower keeps its own signing identity, worker journal and
encrypted keyring. On restart it reads the saved deployment identity from
its home and verifies the host again. The route must still be available.
Transport publication also requires membership in the account's device group.

## Read the documents assigned to a node

On an embedded Vera home, use the receiving node's local `ConfigAccess`.
Defra checks its `DacStatus` permission and requires the query subject to match
the authenticated DID. For example, to read this node's inventory from one issuer:

```rust
let assignments = access.certified_assignments(defra_node::AssignmentQuery {
    policy: policy_id,
    resource,
    relation,
    subject: node_did,
    issuer: issuer_did,
    not_before: last_accepted_position,
}).await?;
```

Use `assignments.object_ids` and `assignments.position` from that single answer.
A successful empty set clears the inventory for this scope. If a proof is
unavailable or fails verification, preserve the last accepted inventory. Pass
its position (term, height and block hash) as the floor on the next read.

A workload owner can use `watch_assignments` to reconcile running work. Its
`AssignmentRuntime` adapter supplies the collection-specific signature checks
and start/stop operations. If revision B is withdrawn before the body of C
arrives, the watcher saves C’s certified revision before stopping B. After a
restart it finishes outstanding stops even while the authority is unreachable,
and rejects an older view when reads resume. The applied
inventory advances after the stop succeeds. C can start once its body is verified.
[Receiver API](crates/gents-store/src/config_client/assignment_receiver.rs) ·
[Restart and withdrawal tests](crates/gents-store/src/identity/enrollment_grant/withdrawal_tests.rs)
