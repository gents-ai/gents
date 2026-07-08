# macOS Bash Sandbox Policies

`ToolSelection.bash_mode` chooses the bash tool surface:

- `Off`: no bash tool.
- `ReadOnly`: exposes the read-only `bash` tool with the read-only command allowlist.
- `Unrestricted`: exposes `bash_unrestricted`, which can run arbitrary command strings subject to the selected command policy.

`ToolSelection.command_execution_policy` chooses the runtime command policy:

| Policy | macOS runtime | Process introspection | Writes | Network mode |
| --- | --- | --- | --- | --- |
| `read_only` | no seatbelt; Rust allowlist validation | allowed when the command itself is allowlisted | denied by read-only validation | `disabled` is enforced for known network probes |
| `workspace_write` / `managed_write` | `/usr/bin/sandbox-exec` seatbelt | only same-sandbox process info | allowed under `WRITABLE_ROOT` | `disabled` removes network allow rules |
| `unrestricted` | no seatbelt | allowed by host user permissions, so `ps` and cross-process `lsof` can work | allowed by host user permissions | `disabled` is rejected because it cannot be enforced |

For custom tool selections, `bash_mode: Unrestricted` with no explicit
`command_execution_policy` defaults to `unrestricted`.

The generated `defra-agent init --write` selection pins
`command_execution_policy: workspace_write` on macOS so the default demo path
keeps write-capable bash contained to the configured tool root. Change that
policy to `unrestricted` for host-diagnostics stewards that need `ps`, broad
`lsof`, or other cross-process inspection.

## Configure It

Desktop app:

1. Open Config, then Tool Selections.
2. Enable Bash.
3. Set Bash mode to `Unrestricted`.
4. Set Command policy to `Unrestricted` for host diagnostics, or `Workspace write` for root-contained writes.

CLI:

```bash
defra-agent config tools set \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --selection-id "$TOOL_SELECTION_ID" \
  --enable-bash \
  --bash-mode Unrestricted \
  --command-execution-policy unrestricted
```

Manifest or direct DefraDB document:

```json
{
  "enable_bash": true,
  "bash_mode": "Unrestricted",
  "command_execution_policy": "unrestricted"
}
```

## Read-only allowlist knobs (which field when)

`ToolSelection` has two related fields that shape the ReadOnly bash surface.
They are not aliases — pick by use case:

| Field | Effect on the base allowlist | Granularity | Reach for when |
| --- | --- | --- | --- |
| `command_allowed_argv_prefixes` | Does **not** replace the base. When non-empty, every command must match a prefix (global gate). In ReadOnly mode a matching prefix also admits heads **outside** the base. | Argv prefix (subcommand-precise) | **Extend** the surface with a diagnostic family, or require a precise argv shape; pairs with `command_forbidden_argv_prefixes` |
| `read_only_command_allowlist` | When present **and non-empty**, **replaces** `default_read_only_commands()` wholesale. Absent or empty = keep the hardcoded default (never deny-all). | Whole executable head (`cat`, `journalctl`) | **Narrow** or fully customize the base (e.g. drop `sudo` / `curl`) — prefixes alone cannot remove a default head |

Validation (see `validate_command_policy` / `validate_read_only_command_inner` in
`crates/defra-agent/src/toolset/shared/command.rs`):

1. Forbidden prefixes always win.
2. If `command_allowed_argv_prefixes` is non-empty, the command must match one
   (global gate) — re-include prefixes for every built-in shape you still want.
3. In ReadOnly mode, the command head must be on the base allowlist **or** match
   an allowed prefix; known tools still get argument-level read-only checks
   (`git`, `sed`, `find`, `curl`, …).

Keep both fields: extension-via-prefixes and replace/narrow-base are different
operator needs. Do not use `read_only_command_allowlist` only to add one head
when argv precision matters — prefer prefixes. Do not use prefixes alone when
you need to strip defaults from the base.

### Extend with argv prefixes

Read-only bash has built-in host diagnostics for common steward commands such
as `date`, `hostname`, `uptime`, `df`, `vm_stat`, `ps`, `lsof`, `curl`,
`launchctl`, and `tailscale`.

Operators can add another read-only diagnostic command family by configuring an
allowed argv prefix on the tool selection. A matching allowed prefix authorizes
that command for read-only bash without granting `bash_mode: Unrestricted`.
The allowed-prefix list is still a global argv gate: when it is non-empty,
include prefixes for every built-in read-only command shape the behavior should
keep using. Forbidden prefixes still take precedence.

```json
{
  "enable_bash": true,
  "bash_mode": "ReadOnly",
  "command_execution_policy": "read_only",
  "command_allowed_argv_prefixes": [
    "spctl --assess --type execute"
  ],
  "command_forbidden_argv_prefixes": [
    "spctl --assess --raw"
  ]
}
```

The allowed prefix is an argv prefix, not a shell string. It can also be written
as JSON when an argument contains spaces:

```json
{
  "command_allowed_argv_prefixes": [
    "[\"log\", \"show\", \"--last\", \"5m\"]"
  ]
}
```

### Replace or narrow the base allowlist

To run ReadOnly bash with a custom executable set (including a strict subset of
the defaults), set `read_only_command_allowlist` to the full desired head list:

```json
{
  "enable_bash": true,
  "bash_mode": "ReadOnly",
  "command_execution_policy": "read_only",
  "read_only_command_allowlist": [
    "ls",
    "cat",
    "git",
    "journalctl"
  ]
}
```

That replaces the default base (so `sudo`, `curl`, `launchctl`, etc. are no
longer admit-by-head unless listed). Leave the field absent or empty to keep
the built-in default. Combine with `command_allowed_argv_prefixes` only when you
also need argv-precise admission; remember a non-empty prefix list is a global
gate.

## macOS Seatbelt Profile

The `workspace_write` policy uses a deny-by-default seatbelt profile with:

```scheme
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read)
(allow file-read*)
(allow file-write-data (literal "/dev/null"))
(allow file-write* (subpath (param "WRITABLE_ROOT")))
```

When network mode is not `disabled`, the profile also allows inbound and
outbound network access. Because `process-info*` is scoped to `same-sandbox`,
general `ps` output and broad cross-process `lsof` inspection are expected to
fail under `workspace_write`.
