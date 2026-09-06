# Gents Fixture Host

Minimal downstream Tauri host for the reusable desktop packages.

## Proves

- Distinct bundle id (`com.source-inc.gents-fixture-host`) and product name
- A purple `Indigo Relay` brand slot and semantic-token remap, deliberately
  distinct from the Source green defaults used by Gents Desktop
- `HomePolicy::AppDataDir { subdirectory: "gents-fixture-host/client" }`
- `BootstrapPolicy::PairedRemoteOnly` (no `runtime-admin` / local runtime init)
- Capability grants limited to chat/fleet/operations (no config-write)
- Co-resident `fixture-domain` plugin with its own file-backed JSON home,
  commands, and `fixture-domain://updated` events
- Real bridge session snapshots rendered through the extracted chat components

This is a package/plugin composition fixture, not a complete Amygdala simulation.
The domain plugin is not a second DefraDB node, and CI does not automate a
enrollment/chat/domain journey through the Tauri webview. Native two-store home
isolation is covered separately by `home_isolation`.

## Dev

```bash
# from repo root once npm workspaces exist:
npm install
npm run tauri -- --manifest-path apps/fixture-host/src-tauri/Cargo.toml dev
```

## Isolation test

```bash
cargo test -p gents-desktop-bridge --test home_isolation
```
