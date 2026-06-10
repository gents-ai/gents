# Adapter Projection Fixtures

This directory holds adapter projection fixtures that exercise Defra Agent's
interoperability contracts without requiring external framework runtimes in
normal CI.

## Checked-In Fixtures

`v1/*.envelope.json` files are dependency-light examples for the first adapter
projection targets:

- OpenAI/Codex-style run trace
- LangGraph-style state/history
- multi-agent task/delegation

These fixtures are validated by normal Rust tests in `defra-agent`.

## External Interop Fixtures

For upstream-captured or Docker-generated fixtures, write direct or wrapped
adapter envelope JSON files to any directory and run:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=/path/to/generated/fixtures \
  cargo test -p defra-agent --test e2e_runtime adapter_projection_external_fixtures -- --ignored --nocapture
```

The same harness can be pointed at the checked-in fixtures:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=scripts/adapter-interop/v1 \
  cargo test -p defra-agent --test e2e_runtime adapter_projection_external_fixtures -- --ignored --nocapture
```

The harness accepts either a direct adapter envelope:

```json
{
  "projection_id": "langgraph_state_history",
  "projection_version": "v1",
  "source_request_id": "request-from-generator",
  "redaction_mode": "full",
  "provenance": {
    "runtime": "defra-agent",
    "source_projection_id": "run_timeline",
    "source_projection_version": "v1"
  },
  "output": {
    "adapter": "langgraph_state_history",
    "projection": {
      "checkpoint_id": "checkpoint-from-generator",
      "root_request_id": "request-from-generator",
      "values": {
        "request_id": "request-from-generator"
      },
      "nodes": [
        {
          "id": "request:request-from-generator",
          "kind": "request"
        }
      ],
      "edges": [],
      "tasks": []
    }
  }
}
```

or a wrapped capture with source metadata:

```json
{
  "source": {
    "system": "langgraph",
    "generator": "docker",
    "version": "captured-by-generator"
  },
  "envelope": {
    "...": "adapter projection envelope"
  }
}
```

The test validates each fixture against:

- the adapter projection DTO contract;
- the generated adapter envelope JSON Schema;
- generated adapter JSONL records and their schema;
- generated eval/sample JSONL records and their schema.

## Native Capture Roundtrip

Wrapped captures may also include a `mapping` block that describes how native
framework evidence maps into Defra runtime documents. The ignored CLI
roundtrip harness imports those mapped native captures into embedded DefraDB,
runs the real `defra-agent trace project` binary, writes JSON/JSONL/eval-JSONL
exports, and lets framework-side verifiers consume those Defra exports:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES=/path/to/generated/fixtures \
DEFRA_AGENT_ADAPTER_INTEROP_EXPORTS=/path/to/generated/fixtures/defra-exports \
  cargo test -p defra-agent-cli --test cli_adapter_interop_roundtrip -- --ignored --nocapture
```

The Docker interop script runs this roundtrip stage after the envelope contract
validation when Rust validation is enabled. The current LangGraph, AutoGen
AgentChat, CrewAI, and Microsoft Agent Framework captures exercise the full
path: real framework execution, native capture import into Defra runtime rows,
embedded DefraDB persistence, real binary export, and framework-container
verifiers that check the Defra export against native state/messages,
participants, delegations, tool events, JSONL records, and eval samples.

Docker, Python, and framework-specific generators should stay outside the
normal suite and write fixtures into the directory passed through
`DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES`.

For Dockerized generators, bind-mount a host output directory and have the
container write one JSON fixture per captured scenario. Then run the Rust
harness against that output directory. This keeps framework installation and
network access out of the default test suite while still proving that captures
from real external runtimes can be represented by the shared Defra Agent
projection contract. Generators without a `mapping` block are not native Defra
import adapters: they execute the framework, collect native evidence, map it
into a wrapped Defra projection envelope, and then validate that envelope.
Generators with a `mapping` block can additionally feed the native-capture
roundtrip harness.

To run every Dockerized generator and validate the combined output with the
Rust harness:

```sh
scripts/adapter-interop/run_docker_interop.sh
```

The script writes to `/tmp/defra-agent-adapter-interop-fixtures` by default.
Pass a directory as the first argument or set `DEFRA_AGENT_DOCKER_INTEROP_OUT`
to choose another output root. Set `DEFRA_AGENT_DOCKER_INTEROP_KEEP=1` to keep
existing files in that root, or `DEFRA_AGENT_DOCKER_INTEROP_SKIP_RUST=1` to
only generate fixtures without invoking the Rust harness.

The `Adapter Interop` GitHub Actions workflow runs the same Docker suite on
demand with `workflow_dispatch` or by adding the `adapter-interop` label to a
PR. Use it when the PR needs a remote, artifact-backed contract proof using
real LangGraph, AutoGen, CrewAI, and Microsoft Agent Framework runtimes without
adding Docker or Python dependencies to default PR CI.

## Generators

- `generators/langgraph/` builds a Docker image that executes real LangGraph
  `StateGraph` flows, captures `get_state_history`, and writes wrapped
  `langgraph_state_history` fixtures into the mounted output directory. It
  emits a linear retry/delegation graph, a compiled-subgraph graph, and a
  provider-shaped chat-model graph.
- `generators/autogen/` builds a Docker image that executes real AutoGen
  AgentChat teams with deterministic custom agents and writes wrapped
  `multi_agent_task` fixtures into the mounted output directory. It emits both
  a `RoundRobinGroupChat` team fixture and a `Swarm` handoff fixture.
- `generators/crewai/` builds a Docker image that executes a real CrewAI
  sequential `Crew` and hierarchical manager-delegation `Crew` with
  deterministic custom `BaseLLM` instances and writes wrapped
  `multi_agent_task` fixtures into the mounted output directory.
- `generators/microsoft-agent-framework/` builds a Docker image that executes
  a real Microsoft Agent Framework group-chat workflow with deterministic
  custom `BaseChatClient` instances and writes a wrapped `multi_agent_task`
  fixture into the mounted output directory.

The wrapped fixture remains the external-runtime artifact. For mapped captures,
Defra Agent derives runtime rows from the native evidence plus mapping metadata
and then exports adapter views through the normal binary path.
