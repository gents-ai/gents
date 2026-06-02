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

The generated `defra-agent init --write-tools` selection pins
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

## Read-Only Diagnostic Extensions

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
