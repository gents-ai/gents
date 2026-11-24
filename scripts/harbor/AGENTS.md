# Harbor / Terminal-Bench

`gents_agent.py` runs Gents as a Harbor custom agent
(`scripts.harbor.gents_agent:GentsAgent`) inside each task container. Gents
sees the verifier's `/app` tree, uses its native write and shell tools,
persists the whole request lifecycle, and writes an ATIF v1.7 trajectory to
`/logs/agent/trajectory.json`. Run Harbor from the repository root with
`PYTHONPATH="$PWD"`, so the module stays importable after Harbor changes into
the job directory. `python3 scripts/harbor/test_gents_agent.py` is the check;
CI runs it.

## Inputs

- A Linux `gents` binary with ATIF support, from `GENTS_BINARY_PATH` (a host
  path) or `GENTS_RELEASE_URL` (a release tarball). The adapter never builds
  Gents inside a task: compilation time and network variance would enter the
  score.
- For task images without a glibc loader (some are musl), a Bullseye glibc
  bundle for the binary's architecture in `GENTS_GLIBC_BUNDLE_PATH`, built once
  with `build_glibc_bundle.sh <out.tar.gz>` (x86_64) or
  `build_glibc_bundle_aarch64.sh <out.tar.gz>` (arm64). When it is set, Gents
  always runs through its loader; when it is unset, a trial whose image has no
  glibc loader fails.
- An OpenAI-compatible endpoint reachable from the task containers
  (`GENTS_INFERENCE_URL` or `OPENAI_BASE_URL`). Harbor isolates agent-phase
  network access, so pass its host with `--allow-agent-host`.
- `GENTS_MAX_TOTAL`, the request-wide token allowance; there is no default.

A run passes the variables with `--ae NAME=value`, for example:

```sh
DOCKER_DEFAULT_PLATFORM=linux/amd64 PYTHONPATH="$PWD" \
  HARBOR_HEALTHCHECK_URL=http://<inference-host>:8000/v1/models \
  ./scripts/harbor/run_with_cleanup.sh run \
  -d terminal-bench/terminal-bench-2-1 \
  --agent scripts.harbor.gents_agent:GentsAgent \
  --model "$SERVED_MODEL_ID" \
  --allow-agent-host <inference-host> \
  --ae GENTS_BINARY_PATH=/path/to/gents \
  --ae GENTS_GLIBC_BUNDLE_PATH=/path/to/gents-glibc-bullseye-x86_64.tar.gz \
  --ae GENTS_INFERENCE_URL=http://<inference-host>:8000/v1 \
  --ae GENTS_MAX_TOTAL=100000
```

Add `--include-task-name <dataset>/<task>` or `--n-tasks 1` for a smoke run.

## Lifecycle

- `run_with_cleanup.sh` wraps Harbor: on interruption it removes only the new
  Harbor `__env` Compose projects, and with `HARBOR_HEALTHCHECK_URL` set it
  polls the endpoint (`HARBOR_HEALTHCHECK_INTERVAL_SECS`, default 15) and stops
  Harbor after `HARBOR_HEALTHCHECK_FAILURE_LIMIT` (default 3) consecutive
  failures.
- Each attempt gets a UUID-suffixed home under `/tmp`, so a cancelled attempt's
  open store cannot affect a retry.
- `run_gents.sh` supervises the server and the response waiter as one
  lifecycle. If the server exits during a request, the waiter is cancelled and
  the runner reports the exit code or signal, then captures bounded
  diagnostics before the home is removed.
- Every trial installs and verifies a `gents-fs-runner` shim and starts the
  server with `--fs-runner`: under the glibc loader `/proc/self/exe` names the
  loader, so the server cannot start its embedded runner.
- Foreground commands default to `GENTS_COMMAND_TIMEOUT_SECS` and are capped at
  `GENTS_COMMAND_TIMEOUT_MAX_SECS`, far below the request deadline; a command
  that times out is killed as a process group and returns a `timeout` outcome
  with partial output, and longer work belongs in `spawn_process`.
- Turn or aggregate-token exhaustion after at least one charged call is
  returned to the verifier as a scoreable budget outcome; missing usage, an
  observed overrun and runtime or provider failures are agent errors.
  `trajectory.json.final_metrics` totals every provider call of the root
  request, compaction included, and the adapter copies them into Harbor's
  `AgentContext` counters.

## Variables

| Variable | Default | Purpose |
|---|---:|---|
| `GENTS_TEMPERATURE` | `1.0` | Sampling temperature |
| `GENTS_TOP_P` | `0.95` | Nucleus sampling |
| `GENTS_TOP_K` | unset | Top-k |
| `GENTS_SEED` | unset | Non-negative sampling seed, persisted on the request and the Harbor result |
| `GENTS_REASONING_EFFORT` | `max` | Reasoning effort |
| `GENTS_MODEL` | Harbor `--model` | Model ID sent to the endpoint |
| `GENTS_DOCKER_PLATFORM` | unset | Platform for task images, e.g. `linux/amd64` |
| `GENTS_MAX_OUTPUT` | `393216` | Per-turn output ceiling, clamped to the context left after the assembled input (the name avoids Harbor's secret-key `TOKEN` heuristic) |
| `GENTS_MAX_TOTAL` | required | Input and output allowance for the whole request; every completed provider call is charged, including tool turns, compaction and retracted attempts, and title inference is off |
| `GENTS_CONTEXT_WINDOW` | `458752` | Prompt and compaction budget |
| `GENTS_MAX_TURNS` | `1000` | Completion-loop turn ceiling |
| `GENTS_RETRY_MAX_TRANSPORT` | `3` | Transient inference retry ceiling |
| `GENTS_REQUEST_TIMEOUT_SECS` | `86400` | Request and Harbor exec deadline |
| `GENTS_COMMAND_TIMEOUT_SECS` | `600` | Foreground command timeout when the model omits `timeout_secs` |
| `GENTS_COMMAND_TIMEOUT_MAX_SECS` | `3600` | Cap on an explicit `timeout_secs` |
| `GENTS_DIAGNOSTIC_TIMEOUT_SECS` | `10` | Per-command ceiling while capturing failure diagnostics |
| `GENTS_DIAGNOSTIC_HOME_MAX_BYTES` | `67108864` | Largest home archived with the diagnostics; a larger one keeps only an inventory |
| `GENTS_SUPERVISION_POLL_SECS` | `1` | Supervisor poll interval |
| `GENTS_TOOL_ROOT` | `/app` | Filesystem and shell tool root |

## Retained per trial

- `trajectory.json`: the ATIF trajectory.
- `request.json`, `request.stdout.json`, `response.json`.
- `gents-init.json`, `gents-profile.json`, `gents-tools.json`,
  `gents-tools-explain.json`, `gents-status.json`, `gents-server.log`,
  `gents-outcome.json`.
- On failure: `gents-diagnostic.json`, `gents-server-exit.json` (when the
  server was reaped), `gents-server-tail.txt`, `process-tree.txt`, the final
  status or `graphql-unavailable.txt`, the partial response, timeline and ATIF
  output, `gents-home-inventory.txt`, and `gents-home.tar.gz` when the home
  fits `GENTS_DIAGNOSTIC_HOME_MAX_BYTES`.
