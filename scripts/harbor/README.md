# Harbor / Terminal-Bench

This adapter runs Gents as a Harbor custom agent inside each task container.
The Gents process sees the same `/app` working tree as the verifier, uses native
write and shell tools, persists the complete request lifecycle, and writes an
ATIF v1.7 trajectory to `/logs/agent/trajectory.json`.

Run commands from the Gents repository root so Harbor can import
`scripts.harbor.gents_agent:GentsAgent`.

## Requirements

- Harbor with Docker access.
- A Linux `gents` binary containing the ATIF projection from PR #988. During
  development, pass its host path with `GENTS_BINARY_PATH`. Once this lands in
  a release, `GENTS_RELEASE_URL` may point at the x86_64 Linux release tarball.
- For a full Terminal-Bench run, a Bullseye glibc compatibility bundle matching
  the Linux binary architecture. Some task images are musl-based, so they cannot
  execute a dynamically linked glibc binary without its loader and libraries. Pass the bundle with
  `GENTS_GLIBC_BUNDLE_PATH`; the adapter installs it only when the image lacks a
  glibc loader.
- An OpenAI-compatible chat-completions endpoint reachable from task
  containers.

The adapter intentionally does not build Gents inside every benchmark task.
That would add Rust compilation time and network variance to the agent score.
Build the compatibility bundle once with:

```sh
./scripts/harbor/build_glibc_bundle.sh \
  /absolute/path/to/gents-glibc-bullseye-x86_64.tar.gz
```

For native Linux/arm64 task containers on an Apple Silicon controller, use:

```sh
./scripts/harbor/build_glibc_bundle_aarch64.sh \
  /absolute/path/to/gents-glibc-bullseye-aarch64.tar.gz
```

## DeepSeek V4 Flash on workstation-1

The workstation service exposes model ID `d4f`. The official DeepSeek code-agent
evaluation uses `reasoning_effort=max`, `temperature=1.0`, and `top_p=0.95`;
those are the adapter defaults.

```sh
DOCKER_DEFAULT_PLATFORM=linux/amd64 PYTHONPATH="$PWD" \
  HARBOR_BIN=/absolute/path/to/harbor \
  HARBOR_HEALTHCHECK_URL=http://100.73.235.38:8000/v1/models \
  ./scripts/harbor/run_with_cleanup.sh run \
  -d terminal-bench/terminal-bench-2-1 \
  --agent scripts.harbor.gents_agent:GentsAgent \
  --model d4f \
  --n-concurrent 16 \
  --n-concurrent-agents 16 \
  --timeout-multiplier 1000 \
  --agent-timeout-multiplier 1000 \
  --max-retries 0 \
  --delete \
  --allow-agent-host 100.73.235.38 \
  --ae GENTS_BINARY_PATH=/absolute/path/to/gents \
  --ae GENTS_GLIBC_BUNDLE_PATH=/absolute/path/to/gents-glibc-bullseye-x86_64.tar.gz \
  --ae GENTS_INFERENCE_URL=http://100.73.235.38:8000/v1 \
  --ae GENTS_DOCKER_PLATFORM=linux/amd64 \
  --ae GENTS_CONTEXT_WINDOW=510976 \
  --ae GENTS_MAX_OUTPUT=393216 \
  --ae GENTS_MAX_TURNS=250 \
  --ae GENTS_REQUEST_TIMEOUT_SECS=86400 \
  --ae GENTS_COMMAND_TIMEOUT_SECS=86400
```

The `1000` multipliers effectively disable Harbor's environment-build, agent
setup, agent execution, and verifier deadlines. The explicit 24-hour Gents
limits serve the same purpose inside the runtime. The Studio Two controller has
32 CPUs and a 256 GiB OrbStack memory allocation. Concurrency 16 keeps the
workstation inference service busy while leaving enough aggregate KV-cache
headroom for typical Terminal-Bench trajectories. The workstation's measured
2,638,151-token KV pool provides about 164,884 live tokens per request at c=16,
or 5.15 fully populated 512K sequences; unusually long generations will queue
or preempt rather than behaving like infinite KV.

`run_with_cleanup.sh` contains controller-level cancellation: Harbor normally
runs `docker compose down` after each trial, and the wrapper removes only new
Harbor `__env` Compose projects if the controller is interrupted. Each Gents
attempt also gets a UUID-suffixed `/tmp` home, so a cancelled attempt's RocksDB
lock cannot affect a retry. When `HARBOR_HEALTHCHECK_URL` is set, the wrapper
also checks the inference service every 15 seconds and stops Harbor after three
consecutive failures; both values can be changed with
`HARBOR_HEALTHCHECK_INTERVAL_SECS` and `HARBOR_HEALTHCHECK_FAILURE_LIMIT`.

For task images that need the glibc compatibility loader, the adapter installs
an explicit `gents-fs-runner` shim and verifies it during setup. This keeps the
native filesystem tools available even though Linux reports the explicit glibc
loader, rather than the wrapped Gents executable, as `/proc/self/exe`.

Harbor isolates agent-phase network access. Keep `--allow-agent-host` aligned
with the inference host or the task container will not be able to reach the
model endpoint.

`PYTHONPATH` keeps the repository importable after Harbor changes into the job
directory. For a deterministic smoke run, add a fully qualified filter such as
`--include-task-name terminal-bench/write-compressor`. To take the first
matching task instead, add `--n-tasks 1`. Start the complete 89-task run only
after the smoke task produces a valid trajectory and verifier result.

Useful overrides:

| Variable | Default | Purpose |
|---|---:|---|
| `GENTS_TEMPERATURE` | `1.0` | Request sampling temperature |
| `GENTS_TOP_P` | `0.95` | Request nucleus sampling |
| `GENTS_TOP_K` | unset | Optional request top-k |
| `GENTS_REASONING_EFFORT` | `max` | DeepSeek thinking effort (`low`, `high`, or `max`) |
| `GENTS_MODEL` | Harbor `--model` | Model ID sent to the inference endpoint |
| `GENTS_DOCKER_PLATFORM` | unset | Force task images/builds, e.g. `linux/amd64` |
| `GENTS_GLIBC_BUNDLE_PATH` | unset | glibc loader/library bundle for musl task images |
| `GENTS_MAX_OUTPUT` | `393216` | Per-turn output cap and prompt-builder output reserve, matching DeepSeek's 384K (384 × 1024) `high`/`max` recommendation. The name deliberately avoids Harbor's secret-key `TOKEN` heuristic. |
| `GENTS_CONTEXT_WINDOW` | `510976` | Gents prompt/compaction budget. This leaves a 1,024-token provider-accounting margin below the 512K server limit while preserving the 393,216-token output cap. |
| `GENTS_MAX_TURNS` | `250` | Agent completion-loop turn ceiling |
| `GENTS_RETRY_MAX_TRANSPORT` | `3` | Transient inference retry ceiling |
| `GENTS_REQUEST_TIMEOUT_SECS` | `86400` | Durable request and Harbor exec timeout |
| `GENTS_COMMAND_TIMEOUT_SECS` | `86400` | Foreground shell command ceiling |
| `GENTS_TOOL_ROOT` | `/app` | Filesystem and shell tool root |

Each trial retains:

- `trajectory.json` — Harbor-native ATIF v1.7 trajectory
- `request.json`, `request.stdout.json`, and `response.json` — request and response
- `gents-init.json`, `gents-profile.json`, `gents-status.json`, and
  `gents-server.log` — runtime evidence
