# Native user runtime service

The installed agent is a foreground `gents server` process owned by the user's
OS service manager. The desktop and other frontends connect to it through the
existing runtime interfaces. Closing a window, disconnecting a frontend, or
quitting the desktop does not stop the runtime. Stop and Restart are explicit
runtime operations, also exposed by the desktop menu-bar controls.

## Platform ownership

- macOS: a LaunchAgent in `~/Library/LaunchAgents/ai.gents.runtime.plist`, in
  the logged-in user's launchd domain. This is not a root LaunchDaemon.
- Linux: `gents-runtime.service` in the user's systemd configuration directory;
  all operations use `systemctl --user`. Gents does not enable linger, change
  machine-wide journal settings, or install a root service.
- Other hosts or Linux without a user systemd session: native service controls
  report unsupported/unavailable. The existing foreground `gents server`
  command remains available; there is no hidden custom supervisor fallback.

One native service definition is installed per OS user. Its configured home
identifies the existing local runtime data, not a new Gents principal. A caller
targeting another home must not overwrite, stop, or adopt it. DefraDB DID/ACP,
the existing provisioning helper, reviewed tool ceiling/root, runtime readiness
and pairing checks remain the authorities. A native service's running state
alone does not establish runtime readiness or identity.

## Installation and operations

For an initialized CLI installation:

```sh
gents service install
gents service start --enable
gents service status
gents status
gents service restart
gents service stop
gents service uninstall
```

Installation does not itself start a new service. `start --enable` enables
login startup; `start` can run without enabling it. Explicit Stop disables
login startup by default; `stop --keep-enabled` retains that preference.
The desktop's Stop Agent control uses the keep-enabled form: it stops the
current process without changing the separate Start at login switch, and the
agent may therefore start again at the next login. The desktop switch changes
only OS login enablement and never starts or stops the current process.
Uninstall removes the native service definition, not the user's agent data,
identity, conversations, or prior diagnostic logs. Service control failure
must be reported; a failed stop must not be treated as a completed restart.

Routine desktop Start and Restart preserve an installed definition's executable
and PATH. They do not replace a CLI-installed service with the GUI's environment.
An update at the same executable path needs no definition change. If the
executable moves, explicitly stop the service, run `gents service install
--executable /absolute/path/to/gents`, and start it again. Installation refuses
to replace a running or transitioning service definition.

The desktop's onboarding keeps its explicit authority review and startup
confirmation. Native OS enablement replaces frontend-owned autostart; opening
a frontend must not restart an intentionally stopped service.
Unsupported desktop platforms offer remote connections rather than a local
service control that cannot work. The foreground CLI remains a separate option.

`make desktop-native-build` builds and stages the same CLI as a Tauri sidecar
using the bundle-specific configuration. Ordinary cargo checks and development
builds do not require a prebuilt sidecar. Local desktop installation also
installs the CLI. The GUI is not repurposed as a second server executable.

## Logging

Existing `tracing` events are sent through maintained native adapters:
`tracing-oslog` on Apple platforms (subsystem `ai.gents`, categories `runtime`
and `desktop`), and `tracing-journald` on Linux. Linux service stdout/stderr
also goes to the journal. Native service definitions set `GENTS_SYSTEM_LOG=1`;
ordinary foreground CLI commands retain stderr output. Desktop diagnostics
fall back to stderr when a native sink is unavailable.

Inspect macOS logs in Console.app or:

```sh
log show --last 1h --predicate 'subsystem == "ai.gents"'
```

Inspect Linux logs with:

```sh
journalctl --user -u gents-runtime.service --since '1 hour ago'
journalctl --user SYSLOG_IDENTIFIER=gents-desktop --since '1 hour ago'
```

Retention belongs to OS policy, not an application file quota. Old desktop log
files are left untouched. There is no custom rotating writer, child-output
collector, locking/rename protocol, or log-supervisor handshake. Raw macOS
stdout/stderr is not automatically unified logging: application diagnostics
must use tracing, and native crash reports remain an OS facility.

## Validation boundaries

Tests must use temporary installation roots and injected native-command
results; they must never register services on a developer's daily-driver
account. Native definition syntax, argument escaping, authority mismatch,
stop/restart errors, and GUI-independent lifetime need explicit coverage.
Readiness continues to use the existing runtime endpoint, not a second health
protocol. OS-host plumbing preserves the modeled process lifecycle: orderly
shutdown of the old process, then startup of a new process. It does not add
host identity, an application process lease, or a new request lifecycle.

Mock tests and syntax validation are not evidence of a signed installer or an
end-to-end login/reboot test. Those platform checks must be reported separately.
