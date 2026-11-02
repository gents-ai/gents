# Upgrading a v1.0.0-rc0 node

An RC0 home upgrades in place: the first start of a newer `gents` runs the
schema migrations in `crates/gents-migration` and logs one line such as

```
ensure_migrations complete baseline_already_present=86 steps_applied=3 ...
```

Later starts report `steps_applied=0`. No manual migration command exists or is
needed. The first start rewrites existing documents onto the new versions: on a
4.4 GB store with about 71,000 affected documents it took 2m18s
before the node reported ready (later starts: about 70 seconds).

## Before you start

- **Upgrade every node and every desktop client in one window.** After the
  upgrade a node's `InferenceBackend` and `Node` collections are on new schema
  versions. Peers and desktops still on RC0 cannot merge what upgraded peers
  write to those, and the desktop refuses to sync with a runtime whose
  replicated schema differs from its own. Transcripts (`AgentMessage`) and the
  other client-authored collections keep their RC0 versions, so phones keep
  working throughout.
- **Client-authored collections keep their RC0 schema.** Phones and apps
  register `AgentMessage`, `AgentRequest`, `AgentSession` and the other
  client-authored collections from the SDL they were built with, so those never
  change: every node, desktop and phone, RC0 or this release, holds the same
  versions. The provider request id v1.0.0-rc1 stored as an `AgentMessage`
  column is carried inside its `publication` JSON instead, and is not written
  yet (an RC0-era reader refuses the extra key); this release reads it either
  way.
- **One canonical lineage for the rest.** DefraDB derives a patched schema
  version from the version it patches, so a fresh store that registered the new
  SDL directly would never match an upgraded RC0 store. This build registers the
  RC0 schema of every runtime-authored collection changed since RC0
  (`InferenceBackend`, `Node`) on a fresh store and replays the same patches
  (`CANONICAL_ROOTS` in `crates/gents-migration/src/upgrades.rs`), so upgraded
  RC0 stores and new stores land on the same versions.
- **Stores v1.0.0-rc1 created or upgraded** (main between 2026-11-02 13:40Z and
  this change) are on a different lineage for `AgentMessage` (and, if rc1
  created them, `InferenceBackend`). They still start, but log an error naming
  each affected collection and how to re-create the store, and report them
  under `non_canonical_lineages` in `gents service update-check` (every managed
  collection) and `/status` (the replicated collections it advertises). A
  desktop that pairs with one says which side is off the lineage instead of
  asking for matching versions. Every node that reported its pins on
  2026-11-02 is on the RC0 pins and upgrades onto the canonical lineage in
  place.
- **workstation-2 runs v0.21.0, not RC0.** Its store holds exactly the RC0
  pins, but its upgrade path from v0.21.0 straight to this release is
  unverified (home layout and config documents were not compared). Take a
  backup of its home first.
- Check `<home>/network.json` matches how the service is started. The managed
  update preflight compares the running endpoint with it and refuses with
  "recorded endpoint must match this home's HTTP listener" when a node was
  started with `--http-port` or `--http-addr` flags that were never persisted.
  `gents service install` persists them.
- A Tools document that sets `integrations.eth_tool_ids` keeps loading; the
  field is ignored with a warning because Ethereum tools left core. Data in
  the removed `EthTool`, `ChainKeyBinding` and `EthSubmission` collections stays
  on disk untouched.

## Upgrade

With the new binary:

```
gents service update-check --home <HOME>   # preflight against the running node
gents service update --home <HOME> ...     # cold backup, swap, restart, verify
```

`update-check` reports `"schema_checked": true` when the running store's
lineage is one this build can migrate, and `"non_canonical_lineages": []` when
it will land on the canonical versions (an RC0 store does). Without the managed path: stop the
service, copy the whole home, replace the binary, start, and look for the
`ensure_migrations complete` line.

## Re-creating an off-lineage store

A node keeps its DID by re-initialising with its existing identity key. Data
only that store holds is not carried over; replicated data comes back from
peers once it re-enrolls.

```
gents service stop                                   # or stop however it runs
mv <HOME> <HOME>.off-lineage
mkdir -p <HOME>/keys
cp <HOME>.off-lineage/keys/<node-name>.key <HOME>/keys/
gents init --home <HOME> --key-path <HOME>/keys/<node-name>.key --node-name <node-name> ...
```

Pass the same `--node-name`, identity backend and backend options the node was
first set up with, then start it and re-enroll it. `/status` should show
`"non_canonical_lineages": []`. For the desktop app, reset its local data.

## Roll back

An RC0 binary refuses a migrated store:

```
... was written by a different Gents version whose schema this release does not know ...
foreign version in collection InferenceBackend
```

Restore the backup taken before the upgrade (the managed update keeps its cold
backup in the operation directory) and start the RC0 binary on it. Anything
written after the upgrade is lost from that node, though peers that already
replicated it keep their copies.
