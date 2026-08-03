# Gents Studio CI runner checklist

Gents CI uses native Apple Silicon because the workspace includes macOS desktop
targets. Each GitHub Actions runner process accepts one job at a time, so a host
needs multiple registered runner processes to run multiple jobs concurrently.

## Current capacity

Both Studios are M3 Ultra hosts with 32 physical cores and 512 GiB of memory.
The intended Gents capacity is two runner processes per host:

| Host | Runner names | Root for the second runner |
| --- | --- | --- |
| `studio-1` | `studio-1`, `studio-1-2` | `/Users/admin/.ghrunner/gents-2` |
| `studio-2` | `studio-2`, `studio-2-2` | `/Users/admin/.ghrunner/gents-2` |

All four registrations must have
`self-hosted,macOS,ARM64,studio,<host>` labels. Confirm the live pool through
GitHub rather than relying on local process state:

```bash
gh api repos/source-inc/gents/actions/runners --paginate \
  --jq '.runners[] | select(.name|startswith("studio-")) | [.name,.status,.busy,([.labels[].name]|join(","))] | @tsv'
```

The durable inventory lives in
`amygdala/infra/services/github-runners/instances/`. Keep both Gents runner
registrations there before using that inventory to reprovision a Studio. Runner
registration tokens are short-lived secrets and must not be committed.

### Supervisor state

The second runner should ultimately be a system LaunchDaemon named
`com.github.actions.runner.gents-2`, matching the first runner's boot behavior.
Inspect it with:

```bash
ssh studio-1 'sudo launchctl print system/com.github.actions.runner.gents-2'
ssh studio-2 'sudo launchctl print system/com.github.actions.runner.gents-2'
```

`studio-2-2` was initially bootstrapped as a per-user LaunchAgent so it could be
brought online without an interactive administrator password. During the next
operator session, promote the already-correct system plist and remove the
temporary user service:

```bash
ssh -t studio-2 '
  launchctl bootout gui/$(id -u)/com.github.actions.runner.gents-2
  sudo launchctl bootstrap system /Library/LaunchDaemons/com.github.actions.runner.gents-2.plist
  sudo launchctl enable system/com.github.actions.runner.gents-2
  sudo launchctl kickstart system/com.github.actions.runner.gents-2
'
```

Verify that the runner returns online in GitHub after promotion. Do this while
it is idle; stopping a runner process cancels its active job.

## CPU allocation

The workflow gives each Cargo process 12 compiler jobs. With two simultaneous
jobs, that allocates at most 24 of 32 cores and leaves eight cores for linking,
the OS, sccache, and other host services. The repository variable can override
this without a workflow edit:

```bash
gh variable set CARGO_BUILD_JOBS --repo source-inc/gents --body 12
```

Watch CPU pressure and job duration before increasing it. Lean can spawn work
outside Cargo's job budget: a measured Rust/Lean overlap drove `studio-1` above
a load average of 100. Do not add a third runner process without first adding a
host-wide CPU admission mechanism or moving Lean to isolated capacity.

## Rust cache safety

Runner processes on a host share `/Users/admin/.cache/sccache`. Each runner
process has its own persistent target directory under
`/Users/admin/.cache/gents-cargo-target/<runner-name>`. A runner process accepts
only one job at a time, so this preserves linked test artifacts without letting
sibling jobs write the same Cargo target tree.

The shared sccache server must be owned by launchd, not by a workflow job.
GitHub runner cleanup kills daemons descended from a completed job; in a
measured run that terminated sccache underneath a sibling compile and forced a
16-minute local CLI link. Install the checked-in LaunchAgent on both hosts:

```bash
scp .github/runner/com.source.gents.sccache.plist \
  studio-1:/Users/admin/Library/LaunchAgents/
scp .github/runner/start-gents-sccache.sh \
  studio-1:/Users/admin/.ghrunner/
scp .github/runner/com.source.gents.sccache.plist \
  studio-2:/Users/admin/Library/LaunchAgents/
scp .github/runner/start-gents-sccache.sh \
  studio-2:/Users/admin/.ghrunner/
```

Only while all Studio runners are idle, replace any workflow-owned daemon:

```bash
ssh studio-1 'chmod 0755 /Users/admin/.ghrunner/start-gents-sccache.sh; launchctl bootout gui/$(id -u)/com.source.gents.sccache 2>/dev/null || true; SCCACHE_DIR=/Users/admin/.cache/sccache /opt/homebrew/bin/sccache --stop-server 2>/dev/null || true; launchctl bootstrap gui/$(id -u) /Users/admin/Library/LaunchAgents/com.source.gents.sccache.plist'
ssh studio-2 'chmod 0755 /Users/admin/.ghrunner/start-gents-sccache.sh; launchctl bootout gui/$(id -u)/com.source.gents.sccache 2>/dev/null || true; SCCACHE_DIR=/Users/admin/.cache/sccache /opt/homebrew/bin/sccache --stop-server 2>/dev/null || true; launchctl bootstrap gui/$(id -u) /Users/admin/Library/LaunchAgents/com.source.gents.sccache.plist'
```

The service also normalizes both runner checkout roots through
`SCCACHE_BASEDIRS`, allowing compiler results to hit across runner instances.
Never stop or restart it from a workflow job: that interrupts sibling compiles.

Check the cache without mutating it:

```bash
ssh studio-1 'SCCACHE_DIR=/Users/admin/.cache/sccache sccache --show-stats'
ssh studio-2 'SCCACHE_DIR=/Users/admin/.cache/sccache sccache --show-stats'
```

If the daemon must be restarted for maintenance, first confirm that every
runner on that host is idle in the GitHub API.

Mathlib is a separate cache surface. Each runner process owns a persistent Lake
directory under `/Users/admin/.cache/gents-lean/<runner-name>`, linked into its
clean checkout before proof or runtime conformance work. The workflow also runs
Mathlib's cache entry point through the Lean interpreter so a new runner
downloads the official precompiled artifacts instead of rebuilding the full
dependency graph. Do not replace that invocation with `lake exe cache get`
while the repository is pinned to Lean 4.18: its LLVM 15-linked executable is
rejected by macOS 26 dyld ([Lean issue #7917](https://github.com/leanprover/lean4/issues/7917)).

## Public-repository security boundary

`source-inc/gents` is public. Pull-request jobs execute repository code on
persistent self-hosted machines, so contributor approval is not an isolation
boundary: an approved fork can modify code run by Cargo, build scripts, tests,
and shell steps. Treat moving pull-request execution into ephemeral macOS VMs
as urgent runner work. Until then, keep credentials and access to sensitive
services off the Studios and do not assume cache or checkout cleanup repairs a
compromised host.

## macOS filesystem checks

Exclude disposable worktrees and the durable compiler cache from Time Machine:

```bash
ssh -t studio-1 'sudo tmutil addexclusion -p /Users/admin/.ghrunner /Users/admin/.cache/sccache /Users/admin/.cache/gents-cargo-target /Users/admin/.cache/gents-lean'
ssh -t studio-2 'sudo tmutil addexclusion -p /Users/admin/.ghrunner /Users/admin/.cache/sccache /Users/admin/.cache/gents-cargo-target /Users/admin/.cache/gents-lean'
```

Confirm the exclusions with `tmutil isexcluded <path>`. For Spotlight, first
measure whether `mds` or `mds_stores` is active during a cold compile:

```bash
ssh studio-1 'ps -axo pid,%cpu,command | egrep "[m]ds(_stores)?"'
ssh studio-2 'ps -axo pid,%cpu,command | egrep "[m]ds(_stores)?"'
```

If indexing is material, add the two runner `_work` directories and the
sccache directory to Spotlight Privacy in System Settings. `mdutil -i off`
changes indexing for an entire volume, so do not use it as a per-directory
exclusion.

Keep Gatekeeper and XProtect enabled. Diagnose them before tuning by checking
whether `syspolicyd` consumes meaningful CPU while Rust is linking tests:

```bash
ssh studio-1 'ps -axo pid,%cpu,command | egrep "[s]yspolicyd|[X]Protect"'
ssh studio-2 'ps -axo pid,%cpu,command | egrep "[s]yspolicyd|[X]Protect"'
```

Finally, runner listeners should have normal scheduling priority (`NI=0`):

```bash
ssh studio-1 'ps -axo pid,ni,state,command | egrep "[R]unner.Listener"'
ssh studio-2 'ps -axo pid,ni,state,command | egrep "[R]unner.Listener"'
```
