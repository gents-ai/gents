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
  cargo test -p defra-agent --test adapter_projection_external_fixtures -- --ignored --nocapture
```

The same harness can be pointed at the checked-in fixtures:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=docs/superpowers/fixtures/adapter-projections/v1 \
  cargo test -p defra-agent --test adapter_projection_external_fixtures -- --ignored --nocapture
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
- generated training/eval JSONL records and their schema.

Docker, Python, and framework-specific generators should stay outside the
normal suite and write fixtures into the directory passed through
`DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES`.

For Dockerized generators, bind-mount a host output directory and have the
container write one JSON fixture per captured scenario. Then run the Rust
harness against that output directory. This keeps framework installation and
network access out of the default test suite while still proving that real
external outputs satisfy the shared Defra Agent projection contract.

## Generators

- `generators/langgraph/` builds a Docker image that executes a real LangGraph
  `StateGraph`, captures `get_state_history`, and writes a wrapped
  `langgraph_state_history` fixture into the mounted output directory.
- `generators/autogen/` builds a Docker image that executes a real AutoGen
  AgentChat `RoundRobinGroupChat` with deterministic custom agents and writes a
  wrapped `multi_agent_task` fixture into the mounted output directory.
