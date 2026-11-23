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
--vera-rpc-port 9841`. Administrative changes require signatures from both
the account and the host Node. Save the `vera_anchor` object from the init
result as `host-anchor.json`. It binds the account, deployment, consensus
key, genesis, device group and host publisher Node.

Start the host. From its account, admit the follower's device signing DID
and save the command's JSON output. Use the group ID from the host's init
result (`transport_group.group`).

```sh
gents runtime p2p enrollment admit-device "$GROUP_ID" "$FOLLOWER_DID" > device-admission.json
```

The admission receipt includes `publisher_node_id`, the registered Node document ID
to use when creating transport records. A direct Defra write must supply this ID.
To create a record on behalf of another Node, the account that owns that Node
must grant the signing device DID the `transport_publisher` relation on it.
Group membership alone grants no such authority. The parent ID is immutable
and stays with the record when another device receives a replica.

Transfer both JSON files to the follower over an authenticated channel.
The signing key stays on the follower. Set up a private tunnel from its
loopback port to the host's Vera loopback port, then initialize a fresh
home with the matching device key and your usual model options.

```sh
gents init --document-acp vera --vera-rpc-port 9841 \
  --vera-anchor host-anchor.json --vera-device-admission device-admission.json \
  --key-path "$FOLLOWER_KEY"
```

The follower verifies the account signature over its Node registration and
checks current group membership and publishing rights against Vera. On
restart it uses the admission saved in its home. The private tunnel must
remain available. To revoke that device’s membership and publisher grant,
run the following command from the host account.

```sh
gents runtime p2p enrollment remove-device "$GROUP_ID" "$FOLLOWER_DID"
```

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

The watcher is a Rust API for workload owners. `gents utils debug fleet slots`
reports a snapshot and does not start a watcher. The restart test uses a real
embedded Vera provider and reopens Defra from disk. Its workload adapter tracks
running IDs and body availability in memory. A fleet integration must supply
the adapter that verifies its signed documents and controls its running work.
[Receiver API](crates/gents-store/src/config_client/assignment_receiver.rs) ·
[Restart and withdrawal tests](crates/gents-store/src/identity/enrollment_grant/withdrawal_tests.rs)
