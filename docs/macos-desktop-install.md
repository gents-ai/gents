# macOS desktop installation

On an Apple Silicon Mac, download `gents-desktop_0.18.0_aarch64.dmg` from the
GitHub release. Open the disk image, drag **Gents** to **Applications**, and launch
it from there. Intel Macs are not included in this release.

The desktop includes its frontend and managed runtime; no Rust, Node, Vite or
separate Gents CLI installation is needed. The `gents-aarch64-apple-darwin.tar.gz`
asset is the separate command-line tool, not the desktop installer.

The app and installer are signed/notarized and carry stapled tickets. Checksums
are in `SHA256SUMS-desktop-macos.txt`. Do not disable Gatekeeper or strip quarantine
to work around an installation failure; report the macOS version and exact error.

Choose a local managed agent, review its tool root and permissions, then connect
inference. Provider accounts and local inference servers are supplied by the user.
Git and language toolchains are needed only for tasks that use them.
