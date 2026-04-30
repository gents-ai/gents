# P2P-only DefraDB subscription management

Date: 2026-04-30

Checkouts inspected:

- `defra-agent`: `6c416e8647e158dcf1fb2bfe635ab97bfb460e61`
- `defradb.rs`: `38202532e6903361b527a46f03eaf7190c4d46e9`
- `defradb`: `f6d89a2d17139601aee9f0dc68f99bb83164bad3`

## Problem

The target deployment shape for `defra-agent` is P2P-first, ideally with only the DefraDB P2P port exposed between nodes. Today, remote subscription setup still depends on the remote node's HTTP API. That means a desktop or another agent can dial a remote peer over P2P, but it cannot ask that remote peer to subscribe collections or install a reverse replicator without also reaching its HTTP/GraphQL endpoint.

The main user-visible consequence is peer pairing. The local desktop can configure its own embedded node directly through `P2POperations`, but reverse pairing a remote runtime currently requires HTTP calls to the remote runtime:

- `crates/defra-agent-desktop-core/src/local_runtime/pairing.rs:13` builds a `reqwest` client from the remote GraphQL URL.
- `crates/defra-agent-desktop-core/src/local_runtime/pairing.rs:32` posts to `/p2p/connect`.
- `crates/defra-agent-desktop-core/src/local_runtime/pairing.rs:33` posts to `/p2p/collections`.
- `crates/defra-agent-desktop-core/src/local_runtime/pairing.rs:39` posts to `/p2p/replicators`.

## Current State

### defra-agent

- The desktop embedded node does local subscription setup on startup. `crates/defra-agent-desktop-core/src/client/core/bootstrap.rs:71` ensures schemas and `:72` calls `subscribe_all_collections`.
- `subscribe_all_collections` is local-only. `crates/defra-agent-desktop-core/src/client/schema.rs:36` gets the embedded node's local P2P handle and `:40` calls `p2p.add_collections`.
- Saved remote peers can include an optional GraphQL endpoint. `crates/defra-agent-desktop-core/src/client/core/bootstrap.rs:178` only performs reverse pairing when `record.graphql` is present, and `:205` to `:219` call the HTTP pairing path.
- Adding a peer follows the same split. `crates/defra-agent-desktop-core/src/client/core/writes.rs:213` installs a local replicator over P2P, then `:243` to `:252` uses the saved GraphQL URL for reverse pairing.
- CLI `p2p` commands are HTTP wrappers:
  - `crates/defra-agent-cli/src/commands/p2p/connect.rs:12` to `:24` posts to `/p2p/connect`.
  - `crates/defra-agent-cli/src/commands/p2p/collections.rs:92` to `:153` lists/adds/removes `/p2p/collections`.
  - `crates/defra-agent-cli/src/commands/p2p/replicators.rs:16` to `:73` lists/adds/removes `/p2p/replicators`.
- `defra-agent server` starts DefraDB with P2P and reloads persisted collection subscriptions, but it does not currently call `subscribe_all_collections` for server runtimes. See `crates/defra-agent-cli/src/commands/serve.rs:108` to `:128` for node startup and `:445` to `:460` for `load_persisted_collections: true`.

### Rust DefraDB (`defradb.rs`)

- Rust DefraDB exposes P2P management as an HTTP-facing local trait. `crates/http/src/router/traits.rs:47` describes `P2POperations` as "P2P operations that can be accessed via HTTP"; `:52` to `:133` includes peer info, connect, replicator, collection, document, and sync operations.
- HTTP routes map that trait onto `/api/v0/p2p/...`. `crates/http/src/router/routes.rs:115` to `:146` registers peer, replicator, collection, document, and sync routes.
- HTTP handlers enforce NAC permissions before calling local P2P operations:
  - `crates/http/src/handlers/p2p/collections.rs:21`, `:42`, `:77`, `:112`, and `:140`.
  - `crates/http/src/handlers/p2p/replicators.rs:65`, `:97`, and `:146`.
- The iroh adapter implements local mutations:
  - `crates/p2p-adapter/src/iroh.rs:198` to `:346` adds a local replicator, persists it, and replays existing docs.
  - `crates/p2p-adapter/src/iroh.rs:421` to `:459` maps collection names to collection IDs, subscribes local topics, and persists local P2P collections.
- The Rust P2P wire protocol does not currently include an admin/control-plane protocol:
  - `crates/p2p/src/protocol.rs:36` to `:62` defines replication, SE, CAR, and identity request/response protocol IDs.
  - `crates/p2p/src/iroh/protocols.rs:32` to `:43` lists accepted iroh ALPNs for pushlog, doc sync, branchable sync, CAR, SE, and two-stream. There is no admin ALPN.
  - `crates/p2p/src/iroh/endpoint_streams.rs:175` to `:330` dispatches those data/sync streams only.
  - `crates/p2p/src/iroh/command.rs:15` to `:155` contains local in-process endpoint commands. `Subscribe`, `CreateReplicator`, and related variants are facade-to-endpoint commands, not remote peer RPCs.

### Go DefraDB (`defradb`)

- Go's public `client.P2P` interface includes the same management surface. `client/p2p.go:25` to `:136` defines peer info, connect, replicators, P2P collections/documents, and sync methods.
- The Go HTTP client uses `/p2p/...` for remote management. `http/client_p2p.go:44` to `:235` covers peer, replicator, and collection management; the rest of the file covers documents and sync endpoints the same way.
- The Go HTTP handler decodes requests and calls local DB/P2P methods. `http/handler_p2p.go:48` to `:180` covers connect, replicators, and collections.
- Go's internal P2P setup wires identity and replication, but not admin management. `internal/db/p2p/p2p.go:183` creates the identity protocol, and `:190` creates `protocol.NewCommChannel(host, "rep", ...)` for pushlog replication. There is no equivalent management channel.
- Go collection subscription is a local mutation. `internal/db/p2p/collection.go:28` to `:75` validates local collections, writes local systemstore keys, and subscribes local pubsub topics on transaction success.
- Go replicator setup is also local. `internal/db/p2p/replicator.go:48` to `:173` persists local replicator state and pushes existing heads to the target peer.
- Go has a reusable request/response pattern that could host a new admin channel. `internal/db/p2p/protocol/comm_channel.go:55` to `:78` builds `/defradb/<name>_req/0.0.1` and `/defradb/<name>_resp/0.0.1` stream handlers.
- Go NAC already gates P2P management in the DB layer. `internal/db/p2p.go` checks node permissions before calling the internal P2P methods, and `tests/integration/acp/nac/*p2p*` covers peer, replicator, collection, document, and sync permissions.

## Conclusion

Remote subscription management is not possible today over the existing DefraDB P2P wire protocol. Both Rust and Go can manage the local node's subscriptions/replicators, and the actual data replication/sync traffic is P2P, but asking a remote node to add/remove/list subscriptions or install a replicator is currently an HTTP control-plane operation.

For `defra-agent`, this means a saved peer with only a P2P address is not enough to fully pair both sides. Without the remote HTTP/GraphQL endpoint, the local side can connect and install its local replicator, but it cannot make the remote runtime subscribe collections or install a reverse replicator.

## Near-term Mitigation

There is a narrow mitigation that reduces the current dependency without solving the general problem:

1. Have `defra-agent server` subscribe the standard agent/runtime collections at startup, immediately after `ensure_runtime_schemas`.
2. Optionally make the startup subscription profile configurable for server deployments.
3. Keep `load_persisted_collections: true` so explicit local subscriptions survive restarts.

This would let known agent runtimes come online already subscribed to the expected collections. It does not solve dynamic reverse pairing, remote replicator installation, collection profile changes, document-level subscriptions, or general remote P2P management.

## Proposed DefraDB Solution

Add an authenticated P2P management RPC to DefraDB. This should live in DefraDB rather than `defra-agent` so both Rust and Go implementations can share the same control-plane semantics and NAC behavior.

Suggested Rust-first shape:

1. Define management request/response messages in `defradb.rs` P2P:
   - Operations: peer info, active peers, connect peer, list/add/remove collections, list/add/remove documents, list/add/remove replicators.
   - Consider whether sync requests belong here too, since they are currently triggered through HTTP even though the sync work is P2P.
   - Include request ID, operation enum, operation payload, actor authentication material, expiry/nonce, and error response fields.
2. Add wire protocols:
   - libp2p: `/defradb/admin_req/0.0.1` and `/defradb/admin_resp/0.0.1`, matching the existing Go comm-channel style.
   - iroh: new ALPNs such as `/defra-iroh/admin/0.1` and `/defra-iroh/admin/0.1/resp`.
3. Authenticate every request:
   - Verify P2P message signatures using the existing P2P signing utilities.
   - Bind the requesting peer to an actor DID before running NAC. Existing libp2p identity request/response can inform this, but iroh appears to need equivalent support or an actor token in each admin request.
   - Peer ID alone should not authorize node administration.
4. Authorize every operation using the same `NodePermission` checks used by HTTP:
   - `P2pPeerConnect`
   - `P2pCollectionAdd/Delete/List`
   - `P2pDocumentAdd/Delete/List`
   - `P2pReplicatorAdd/Delete/List`
   - sync permissions if sync operations are included.
5. Reuse existing local implementations after authorization:
   - The handler should call the same P2P adapter or DB methods that HTTP calls today.
   - Error mapping can differ by transport, but behavior should match HTTP.
6. Make exposure explicit:
   - Default disabled or capability-gated for remote admin.
   - Support an allowlist/capability for bootstrapping trusted peers.
   - Add rate limits, timeouts, request size limits, replay protection, and audit logging.

Go parity can follow the same wire shape:

1. Add a management `CommChannel`, likely `protocol.NewCommChannel(host, "admin", ...)`.
2. Define matching CBOR messages under `internal/db/p2p/message` or `internal/db/p2p/protocol`.
3. `ProcessRequest` should verify the actor identity, run `db.checkNodeAccess`, then call the existing local DB/P2P methods.
4. Preserve the existing HTTP API as a compatibility layer.

## defra-agent Follow-up Work

After DefraDB exposes remote P2P management:

1. Add a `RemoteP2pAdmin` client abstraction in desktop core and CLI.
2. Teach desktop peer pairing to use P2P admin when a peer has only a P2P address, with HTTP fallback during migration.
3. Stop treating GraphQL as required for reverse pairing in saved peer records.
4. Update `defra-agent p2p` CLI commands so they can target either a local HTTP endpoint or a remote P2P peer.
5. Keep the server auto-subscribe mitigation as a startup convenience, not as the primary control-plane design.

## Test Plan

DefraDB Rust:

- Unit-test management message serialization, signing, verification, and tamper failures.
- Integration-test iroh and libp2p management RPCs: node A asks node B to add/list/remove a collection subscription, then verify node B receives replicated updates without HTTP.
- Integration-test remote add/list/remove replicator.
- NAC tests for every management operation: authorized identity succeeds, no identity fails, wrong identity fails, admin relation succeeds.
- Replay and abuse tests: stale request rejected, duplicated idempotent request safe, oversized payload rejected, invalid collection/document/peer returns a structured error.
- Restart tests: remotely added subscriptions and replicators persist and reload.

Go DefraDB:

- Add equivalent comm-channel message tests.
- Add NAC and interop tests once the wire format is shared.
- If cross-runtime support is required, test Rust-to-Go and Go-to-Rust admin requests.

defra-agent:

- Desktop saved peer with only a P2P address can complete reverse pairing.
- Desktop saved peer with GraphQL still works through HTTP fallback.
- `defra-agent server` starts with expected local subscriptions when the near-term mitigation is enabled.
- Failure cases are surfaced clearly: remote peer offline, unauthorized remote admin, unknown collection, and duplicate setup.

## Open Questions

- Should remote P2P admin be enabled by default, or only with an explicit node config/capability?
- What is the right actor authentication shape for iroh: a per-request identity token, an iroh identity exchange equivalent, or a higher-level capability document?
- Which operations are safe for the first version? Collections and replicators may be enough for `defra-agent`; documents and sync can be added after the control-plane foundation is stable.
- Is Go interop required for the first milestone, or can Rust `defradb.rs` ship first with a versioned protocol and Go parity later?
