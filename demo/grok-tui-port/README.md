# Grok TUI port pack

Map the Grok TUI wire from `grok-build`, audit the closed protocol ledger,
fan out eight path-disjoint implementation agents in isolated git worktrees,
directly review every sealed slice in parallel, serial-apply accepted diffs
onto the operator checkout, retry rejected attempts in fresh workspaces, and
give a dedicated convergence agent ownership
of the semantic merge and compile/test commit. The pack then runs the bundled
full **code-review** graph, proves that exact reviewed head with live GLM turns,
and opens one GitHub PR. Small sealed slices use one direct reviewer; the final
combined edge starts the full multi-stage embedded graph.

This pack does not add DefraDB access-control policy and does not implement
Grok permission UI. Threat model is reachability of the Gents server / leader
socket. Workers never `make worktree` or `git commit`; the host creates,
seals, and integrates worktrees.

Gents is the leader-socket server in this port. It binds the Unix socket and
stock `grok --leader --leader-socket <path>` connects as the pager client. The
shim reads Grok `ClientMessage` frames, writes `ServerMessage` frames, and maps
ACP traffic onto Gents documents; it does not launch Grok's own leader process.

```text
GrokPortJob
  -> recon (ceiling: gents + grok-build) -> PortSurface*
  -> recon-audit -> plan -> exactly 8 path-disjoint PortWorkUnits
  -> CallbackBinding CreateWorkspace per unit (8-way fanout)
       IsolatedWorkspace at <gents>/.gents/workspaces/gents-ws-<id>-<branch>
  -> implement ReadWrite -> host seal -> WorkspaceReceipt kind=writer
  -> per-slice review ReadOnly on each actual sealed dirty tree (parallel)
       receipt changed-files + direct untracked-file reads; mapped wire + tests
       zero material findings -> PortUnitClosure accepted
       findings -> PortUnitClosure retry
         preserve review + sealed diff -> new PortWorkUnit attempt
         -> new host worktree -> implement -> seal -> independent review
  -> serial Integrate of accepted closures
       host ApplyDiff onto the operator checkout (one trunk HEAD)
       WorkspaceReceipt kind=integrator
       only that durable receipt -> PortIntegrateResult applied
  -> convergence agent on all 8 applied slices
       reconcile interfaces; fmt; focused test/check; commit exact green HEAD
  -> full bundled code-review graph on the convergence commit
       focused repair commits; pin exact green HEAD
  -> build that exact HEAD in a separate run-owned live home
  -> stock grok --leader live GLM probes with exact surface-ID coverage
  -> live-review fail-closed
  -> publish (unbound): checkout -B <branch>, gh pr create, wait CI,
       verify PR head is the reviewed/live-tested head, never merge
```

There is no synthetic merge worktree. The host advances the operator
`RepositoryPlacement` with serial integrator receipts, then the convergence
agent performs the semantic merge on that one trunk. This separates mechanical
patch application from compiler-driven integration and independent review.
Attempt identity is separate from logical-unit identity: failed seals remain
immutable audit evidence, while only one host-confirmed integration can close
each of the eight logical slots. There is no arbitrary attempt ceiling.

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
(default `agent/grok-tui-port-pack8`).

The default inference pool is workstation-1 at
`http://100.73.235.38:8000/v1`, with one shared 16-request concurrency cap
across coordinators, the eight concurrent implementers, the eight concurrent
sealed reviewers, convergence, and the final review graph.

`make grok-port` verifies its `/models` endpoint advertises
`GLM-5.3-NVFP4` at context length 262144 before it seeds any documents.

Useful controls:

```bash
export GENTS_GROK_PORT_MIN_SURFACES=13
export GENTS_GROK_PORT_MAX_SURFACES=13
export GENTS_GROK_PORT_BASE_SHA=$(git rev-parse HEAD)
export GENTS_GROK_PORT_PR_BASE=main
export GENTS_GROK_PORT_BRANCH=agent/grok-tui-port-pack8
export GENTS_GROK_PORT_PROMPT='Prioritize subagents, interrupts, and model name.'
```

Every run lands under `demo/grok-tui-port/runs/<job-id>/`.

## Live edge probes

`grok -p` does not use leader mode. Test the framed edges independently against
a running integrated server:

```bash
python3 demo/grok-tui-port/scripts/grok_edge_probe.py \
  --socket /tmp/gents-grok-live.sock \
  --graphql http://127.0.0.1:19205/api/v0/graphql \
  --edge handshake
python3 demo/grok-tui-port/scripts/grok_edge_probe.py \
  --socket /tmp/gents-grok-live.sock \
  --graphql http://127.0.0.1:19205/api/v0/graphql \
  --edge prompt
python3 demo/grok-tui-port/scripts/grok_edge_probe.py \
  --socket /tmp/gents-grok-live.sock \
  --graphql http://127.0.0.1:19205/api/v0/graphql \
  --edge tool
python3 demo/grok-tui-port/scripts/grok_edge_probe.py \
  --socket /tmp/gents-grok-live.sock \
  --graphql http://127.0.0.1:19205/api/v0/graphql \
  --edge subprocess
python3 demo/grok-tui-port/scripts/grok_edge_probe.py \
  --socket /tmp/gents-grok-live.sock \
  --graphql http://127.0.0.1:19205/api/v0/graphql \
  --edge cancel
```

`--edge all` runs the same checks on one multi-turn session. Keep one separate
stock `grok --leader --leader-socket <path>` PTY smoke in the final gate.
The integrated server must use `--grok-shim-behavior-id port-live`; the shim
derives its advertised model and context window from that bound behavior.
Pass `--model "$GENTS_GROK_PORT_MODEL"` for a non-default pack model; the probe
also reads that environment variable directly when the flag is omitted.
