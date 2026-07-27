# Gents Fixture Host

Minimal downstream Tauri host for [reusable desktop packages](../../docs/reusable-desktop-packages.md) phase 4.

## Proves

- Distinct bundle id (`com.source-inc.gents-fixture-host`) and product name
- `HomePolicy::AppDataDir { subdirectory: "gents-fixture-host/client" }`
- `BootstrapPolicy::PairedRemoteOnly` (no `runtime-admin` / local runtime init)
- Capability grants limited to chat/fleet (no config-write)
- Co-resident `fixture-domain` plugin with its **own** storage home, commands, and `fixture-domain://updated` events

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
