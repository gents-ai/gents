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
- **Do not add freshly initialised stores to an upgraded RC0 fleet yet.** A
  store created by a newer build (new `gents init`, a new desktop install)
  registers `AgentMessage` at a different version than a store upgraded from
  RC0, because DefraDB derives a patched version from its predecessor. The
  two cohorts cannot sync. `crates/gents-migration/tests/rc0_lineage_parity.rs`
  lists the affected collections; this restriction lifts when that list is
  empty.
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
lineage is one this build can migrate. Without the managed path: stop the
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
