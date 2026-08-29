# Grok TUI port pack

Map the Grok TUI wire from `grok-build`, audit the closed protocol ledger,
implement a Gents-only thin client in isolated git worktrees, directly review
each small sealed route, serial-apply accepted diffs onto the operator checkout
(one trunk), run the bundled full **code-review** graph on the committed
combined result, prove that exact reviewed head with live GLM turns, then open
one GitHub PR.

This pack does not add DefraDB access-control policy and does not implement
Grok permission UI. Threat model is reachability of the Gents server / leader
socket. Workers never `make worktree` or `git commit`; the host creates,
seals, and integrates worktrees.

```text
GrokPortJob
  -> recon (ceiling: gents + grok-build) -> PortSurface*
  -> recon-audit -> plan -> PortWorkUnit* (status=ready only)
  -> CallbackBinding CreateWorkspace per ready unit
       IsolatedWorkspace at <gents>/.gents/workspaces/gents-ws-<id>-<branch>
  -> implement ReadWrite -> host seal -> WorkspaceReceipt kind=writer
  -> one-route review ReadOnly on the actual sealed dirty tree
       git diff <base_sha>; mapped wire + targeted tests
       zero material findings -> PortUnitClosure accepted
       else blocked (no same-workspace rewrite; sealed trees are read-only)
  -> serial Integrate of accepted closures
       host ApplyDiff onto the operator checkout (one trunk HEAD)
       WorkspaceReceipt kind=integrator
  -> full bundled code-review graph on the committed combined trunk
       bounded repair commits; pin exact green HEAD
  -> build that exact HEAD in a separate run-owned live home
  -> stock grok --leader live GLM probes with exact surface-ID coverage
  -> live-review fail-closed
  -> publish (unbound): checkout -B <branch>, gh pr create, wait CI,
       verify PR head is the reviewed/live-tested head, never merge
```

There is no host action that concatenates N worktrees into a new worktree.
One trunk is the operator `RepositoryPlacement`, advanced by serial
integrator receipts — the same mechanism as `demo/repo-maintenance`.

Recon is required to emit at least `attach`, `session`, `model`, `context`,
`tool_call`, `subprocess`, `subagent`, and `interrupt`. Each `PortSurface`
carries a self-contained `grok_wire` packet; later stages cannot open
grok-build.

## Run

```bash
make grok-port
```

Expects grok-build at `/Users/johnzampolin/go/src/github.com/xai-org/grok-build`.
Override with `GROK_PORT_GROK_ROOT` / `GROK_PORT_CEILING`. Pin the workspace
base with `GROK_PORT_BASE_SHA`. The PR head is `GROK_PORT_BRANCH`
(default `agent/grok-tui-port`).

The default inference pool is workstation-1 at
`http://100.73.235.38:8001/v1`, with one shared 32-request concurrency cap
across coordinators, implementers, and reviewers.

`make grok-port` verifies its `/models` endpoint advertises
`GLM-5.3-Flash-NVFP4` at context length 524288 before it seeds any documents.

Useful controls:

```bash
export GENTS_GROK_PORT_MIN_SURFACES=8
export GENTS_GROK_PORT_MAX_SURFACES=16
export GENTS_GROK_PORT_BASE_SHA=$(git rev-parse HEAD)
export GENTS_GROK_PORT_PR_BASE=main
export GENTS_GROK_PORT_BRANCH=agent/grok-tui-port
export GENTS_GROK_PORT_PROMPT='Prioritize subagents, interrupts, and model name.'
```

Every run lands under `demo/grok-tui-port/runs/<job-id>/`.
