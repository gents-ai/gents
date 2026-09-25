# Gents

Gents is a desktop app for running your own AI agents. Each agent runs in the
background on your machine and keeps its configuration, conversations, and
work as documents in a local, replicated database.

## Install

Download the installer for your platform from the
[latest GitHub release](https://github.com/gents-ai/gents/releases/latest).
The app includes everything it needs; you do not need to install Rust, Node, or
a separate command-line tool.

### macOS (Apple Silicon)

1. Download `gents-desktop_<version>_aarch64.dmg`.
2. Open the disk image and drag **Gents** to **Applications**.
3. Launch **Gents** from **Applications**. Keep it there so the background
   agent can find it at the next login.

Intel Macs are not supported. The app is signed and notarized. If macOS refuses
to open it, do not disable Gatekeeper or strip the quarantine attribute;
[open an issue](https://github.com/gents-ai/gents/issues) with your macOS
version and the exact error.

### Linux (x86_64)

On Debian 12, Ubuntu, or a compatible newer distribution:

```sh
sudo apt install ./gents-desktop_<version>_amd64.deb
```

Then launch **Gents** from your application menu.

Or use the AppImage (glibc 2.36 or newer):

```sh
chmod +x gents-desktop_<version>_x86_64.AppImage
./gents-desktop_<version>_x86_64.AppImage
```

If FUSE is unavailable, run it with `APPIMAGE_EXTRACT_AND_RUN=1`. The
background agent needs a user systemd session.

Installer checksums are attached to each release as
`SHA256SUMS-desktop-macos.txt` and `SHA256SUMS-desktop-linux.txt`.

### Server / CLI

For a headless runtime, download `gents-<target>.tar.gz` (Linux x86_64/aarch64,
macOS arm64) from the same release; checksums are in `SHA256SUMS-cli-*.txt`.

## First run

The app walks you through setup: create a local agent, review the folders and
permissions it may use, then connect a model provider. If `~/.gents` already
exists, setup continues that agent instead of creating a second one.

The agent keeps running when you close the window or quit the app. Use
**Stop Agent** in the menu bar or tray menu to stop it; **Start at login** is a separate setting.

## Get help

Report problems and questions as
[GitHub issues](https://github.com/gents-ai/gents/issues). Include your OS and
version, the Gents version, and the step that failed. Logs help: on macOS, open
Console.app and filter by subsystem `ai.gents`; on Linux, run
`journalctl --user -u gents-runtime.service`. Never include credentials, API
keys, or sign-in callback URLs.

## Contributing

Building from source is covered in [DEVELOPMENT.md](DEVELOPMENT.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
