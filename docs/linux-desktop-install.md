# Linux desktop installation

Download the **gents-desktop** `.deb` or AppImage for x86_64/amd64 from the
GitHub release. That release does not include a separate command-line archive.

For Debian 12 or a compatible newer Debian/Ubuntu desktop:

```sh
sudo apt install ./gents-desktop_0.18.5_amd64.deb
```

Launch **Gents** from your application menu, or run `gents-desktop-tauri`.
The app includes its managed runtime and frontend; no Rust, Node, Vite or separate
Gents CLI installation is required. If `~/.gents` already exists, setup continues
that agent and its user systemd unit instead of creating a second identity. A user systemd session is required for the
local background agent; Gents does not install a root service or enable linger.
A desktop session and browser are needed for
interactive onboarding/provider sign-in. Git and language toolchains are separate
requirements for tasks that use them, not for opening onboarding.

Alternatively, on x86_64 Linux with glibc 2.36 or newer:

```sh
chmod +x gents-desktop_0.18.5_x86_64.AppImage
./gents-desktop_0.18.5_x86_64.AppImage
```

AppImage support still depends on host graphics/display facilities. If FUSE is
unavailable, use `APPIMAGE_EXTRACT_AND_RUN=1 ./gents-desktop_0.18.5_x86_64.AppImage`.
Checksums are supplied in `SHA256SUMS-desktop-linux.txt`.

An AppImage's mount is temporary, so the app copies its runtime to
`~/.local/share/gents/desktop/runtime/gents`, and the user service runs that
copy. The copy needs about 130 MB. No extraction step is required, and the
AppImage can be moved or renamed afterwards.

Opening a newer AppImage refreshes that copy. When the background agent is
stopped, the app also updates the installed service definition. An agent that
is already running keeps its current definition until you restart it from the
app, which writes the new definition before starting it again.

Choose a local managed agent, review its tool root and authority, then connect an
inference provider. Local inference must be reachable from your own machine;
internal workstation hostnames are not preconfigured in the shipped app.

The OS runs the agent independently of the desktop. Closing a window or choosing
Quit Desktop leaves it running. Use Stop Agent to stop it; Start at login is a
separate preference. Diagnostics are available in the user journal, for example
`journalctl --user -u gents-runtime.service`. OS policy controls log retention.

Release automation verifies package contents/dependencies and a fresh-user startup
under a virtual Linux display. This is not a claim of full OAuth or end-to-end
onboarding acceptance on every Linux distribution. Report distribution/version,
desktop environment, release version and the failing step when reporting problems;
do not include credentials or authorization callback URLs.
