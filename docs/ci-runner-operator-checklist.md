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

Watch CPU pressure and job duration before increasing it. The next sensible
experiment is three runners at eight compiler jobs each, not three runners at
12 jobs each.

## Rust cache safety

Runner processes on a host share `/Users/admin/.cache/sccache`, but each job
must use a disposable Cargo target beneath `runner.temp`. Never stop or restart
the shared sccache daemon from a job: that interrupts sibling compiles.

Check the cache without mutating it:

```bash
ssh studio-1 'SCCACHE_DIR=/Users/admin/.cache/sccache sccache --show-stats'
ssh studio-2 'SCCACHE_DIR=/Users/admin/.cache/sccache sccache --show-stats'
```

If the daemon must be restarted for maintenance, first confirm that every
runner on that host is idle in the GitHub API.

## macOS filesystem checks

Exclude disposable worktrees and the durable compiler cache from Time Machine:

```bash
ssh -t studio-1 'sudo tmutil addexclusion -p /Users/admin/.ghrunner /Users/admin/.cache/sccache'
ssh -t studio-2 'sudo tmutil addexclusion -p /Users/admin/.ghrunner /Users/admin/.cache/sccache'
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
