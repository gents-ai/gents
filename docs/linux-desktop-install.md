# Linux desktop installation

Download the **gents-desktop** asset for x86_64/amd64 from the GitHub release.
The separate `gents-x86_64-unknown-linux-gnu.tar.gz` is the command-line runtime,
not the desktop application.

For Debian 12 or a compatible newer Debian/Ubuntu desktop:

```sh
sudo apt install ./gents-desktop_0.18.0_amd64.deb
```

Launch **Gents** from your application menu, or run `gents-desktop-tauri`.
The app includes its managed runtime and frontend; no Rust, Node, Vite or separate
Gents CLI installation is required. A desktop session and browser are needed for
interactive onboarding/provider sign-in. Git and language toolchains are separate
requirements for tasks that use them, not for opening onboarding.

Alternatively, on x86_64 Linux with glibc 2.36 or newer:

```sh
chmod +x gents-desktop_0.18.0_x86_64.AppImage
./gents-desktop_0.18.0_x86_64.AppImage
```

AppImage support still depends on host graphics/display facilities. If FUSE is
unavailable, use `APPIMAGE_EXTRACT_AND_RUN=1 ./gents-desktop_0.18.0_x86_64.AppImage`.
Checksums are supplied in `SHA256SUMS-desktop-linux.txt`.

Choose a local managed agent, review its tool root and authority, then connect an
inference provider. Local inference must be reachable from your own machine;
internal workstation hostnames are not preconfigured in the shipped app.

Release automation verifies package contents/dependencies and a fresh-user startup
under a virtual Linux display. This is not a claim of full OAuth or end-to-end
onboarding acceptance on every Linux distribution. Report distribution/version,
desktop environment, release version and the failing step when reporting problems;
do not include credentials or authorization callback URLs.
