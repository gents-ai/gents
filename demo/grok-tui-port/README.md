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
  --edge subagent
python3 demo/grok-tui-port/scripts/grok_edge_probe.py \
  --socket /tmp/gents-grok-live.sock \
  --graphql http://127.0.0.1:19205/api/v0/graphql \
  --edge cancel
```

Subagent lifecycle (extension rail `x.ai/session_notification`, camelCase
`sessionId`/`update`/`_meta` envelope, snake_case variant fields under the
`sessionUpdate` tag): the finished DTO's exact serde schema requires
`subagent_id`, `child_session_id`, `status` (exactly one of
`completed|failed|cancelled`), `tool_calls`, `turns`, `duration_ms`,
`tokens_used`, `will_wake`; `error`/`output` are optional (absent, null, or
string); `parent_session_id` is absent; `subagent_id == child_session_id`.
The schema validator is outcome-neutral — a well-formed `failed` or
`cancelled` DTO is still an exact DTO. The live foreground success edge
then separately requires `status == "completed"` and `will_wake: false`,
so a valid failed DTO is recognized as well-formed but still fails that
edge. The probe also validates the exact spawned DTO (required
`sessionUpdate`, `subagent_id`, `parent_session_id`, `child_session_id`,
`subagent_type`, `description`; serde-option extras
`parent_prompt_id`, `effective_context_source`, `context_normalized`
(false is omitted on the wire), `capability_mode`, `persona`, `role`,
`model`, `resumed_from`, `workflow_run_id`) and the exact all-required
progress DTO (`duration_ms`, `turn_count`, `tool_call_count`,
`tokens_used`, `context_window_tokens`, `context_usage_pct` bounded to
[0, 100], `tools_used`, `error_count`), rejecting legacy camelCase names
and unknown extras. An inline fixture self-test calls these same real
validators before the live envelopes are trusted. Client request/result
methods (`x.ai/subagent/get`, `x.ai/subagent/cancel`,
`x.ai/subagent/list_running`) keep their separately audited camelCase DTO
shapes. The worker target `port-live-worker` is
no-shell/no-file/no-subagent; its parent target `port-live-tools` is
foreground-only (`subagent_default_await_mode: "foreground"`,
`subagent_background_enabled: false`).

`--edge all` runs the same checks on one multi-turn session. Keep one separate
stock `grok --leader --leader-socket <path>` PTY smoke in the final gate.
The integrated server must use `--grok-shim-behavior-id port-live`; the shim
derives its advertised model and context window from that bound behavior.
Pass `--model "$GENTS_GROK_PORT_MODEL"` for a non-default pack model; the probe
also reads that environment variable directly when the flag is omitted. The
same applies to the context window: the probe's standalone default is 262144,
and `--context-window` (or `GENTS_GROK_PORT_CONTEXT_WINDOW`, read directly
when the flag is omitted) overrides it for a non-default pack profile.
