# Upgrading a v1.0.0-rc0 node

An RC0 home upgrades in place: the first start of a newer `gents` runs the
schema migrations in `crates/gents-migration` and logs one line such as

```
ensure_migrations complete baseline_already_present=86 steps_applied=2 ...
```

Later starts report `steps_applied=0`. No manual migration command exists or is
needed.

## Before you start

- **Upgrade every node and every desktop client in one window.** After the
  upgrade a node's `AgentMessage` and `InferenceBackend` collections (and
  `Node`, once the avatar change ships) are on a new schema version. Peers
  still on RC0 cannot merge what upgraded peers write, and the desktop refuses
  to sync with a runtime whose replicated schema differs from its own.
- **One canonical lineage.** DefraDB derives a patched schema version from the
  version it patches, so a fresh store that registered the new SDL directly
  would never match an upgraded RC0 store. This build registers the RC0 schema
  of every collection changed since RC0 on a fresh store and replays the same
  patches (`CANONICAL_ROOTS` in `crates/gents-migration/src/upgrades.rs`), so
  upgraded RC0 stores and new stores land on the same versions.
- **Stores created by v1.0.0-rc1** (main between 2026-11-02 13:40Z and this
  change) registered the new SDL directly and are on a different lineage. They
  still start, but log a warning and report the affected collections under
  `non_canonical_lineages` in `/status` and in `gents service update-check`.
  They cannot sync those collections with canonical peers; re-create them (a
  new desktop store or `gents init`) to rejoin.
- **Phones** that apply bundled SDL through the FFI must replay
  `gents_migration::canonical_schema_plan(collection)` for `AgentMessage`
  (register the rc0 root SDL, then apply each patch and make it active) instead
  of registering the current SDL.
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
