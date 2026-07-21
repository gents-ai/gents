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

The generated `gents init --write` selection pins
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
gents config tools set \
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
| `command_allowed_argv_prefixes` | Does **not** rewrite the base list. When non-empty, every command must match a prefix (global gate — can drop default heads that do not match). In ReadOnly mode a matching prefix also admits heads **outside** the base. | Argv prefix (subcommand-precise) | Require a precise argv shape / admit a non-default diagnostic family; pairs with `command_forbidden_argv_prefixes` |
| `read_only_command_allowlist` | When present **and non-empty**, **replaces** `default_read_only_commands()` wholesale. Absent or empty = keep the hardcoded default (never deny-all). | Whole executable head (`cat`, `journalctl`) | **Narrow** or fully customize the base under allowlist admission (empty prefixes). Prefixes cannot surgically edit that base list. |

Validation (see `validate_command_policy` / `validate_read_only_command_inner` in
`crates/gents/src/toolset/shared/command.rs`):

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

Operators can authorize another read-only diagnostic command family with an
allowed argv prefix — without granting `bash_mode: Unrestricted`. A matching
prefix admits that argv shape even when the executable is not on the base
allowlist. Forbidden prefixes still take precedence.

**Global-gate caveat:** a non-empty `command_allowed_argv_prefixes` list
requires **every** command to match a prefix. The snippet below alone does
**not** mean “defaults plus `spctl`”; it admits only matching `spctl` argv and
drops default heads like `date` / `ls` that do not match. To keep built-ins,
either leave this field empty (and use `read_only_command_allowlist` to change
the base), or re-include prefixes for every built-in shape you still want.

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
the defaults), set `read_only_command_allowlist` to the **full** desired head
list — this replaces the hardcoded default wholesale:

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

With that document, `ls` / `cat` / `git` / `journalctl` are admit-by-head;
default heads not listed (`sudo`, `curl`, `date`, `launchctl`, …) are not.
Leave the field absent or empty to keep the built-in default (empty never
means deny-all).

To add a whole-executable head while keeping most defaults, copy the default
set into `read_only_command_allowlist` and append the new head — do not set a
lone prefix unless you also intend the global prefix gate. Combine with
`command_allowed_argv_prefixes` only when you also need argv-precise admission
on top of that base.

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
