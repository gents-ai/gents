# Downstream update workflow

Supported update for hosts that depend on Gents desktop packages (Amygdala, fixture host, etc.):

1. **Bump pins in lockstep** to the same `vX.Y.Z` tag:
   - Rust: `gents-desktop-bridge = { git = "…", tag = "vX.Y.Z" }`
   - npm: `@source-inc/gents-desktop-*` exact versions (GitHub Packages or release tarball URLs)
2. **Read** root `CHANGELOG.md` → Bridge contract section for that release.
3. **Run** your contract + e2e + visual gates (copy from `apps/fixture-host` + Gents CI).
4. **Merge**.

`contract_version` is `MAJOR.MINOR` independent of the release version:

- MINOR = additive (new commands, optional fields, new error codes)
- MAJOR = breaking (rename/removal/shape change)

The TS client accepts same MAJOR and MINOR ≥ build-time requirement via
`desktop_bridge_contract`.
