# Adapter interop

Docker generators that run real external agent frameworks, capture their
native evidence and write adapter projection fixtures, so the Gents adapter
projection contract is checked against real runtimes without putting Docker,
Python or framework installs in the default test suite.

The checked-in envelopes in
`crates/gents/tests/fixtures/adapter_projections/envelopes/` are validated by
the ordinary `gents-observe` tests; nothing here runs in them.

## Fixtures

A generator writes one JSON file per scenario into the directory mounted at
`/out`: either a direct adapter envelope (`projection_id`,
`projection_version`, `source_request_id`, `redaction_mode`, `provenance`,
`output`) or a wrapped capture (`source` with `system`, `generator`,
`version`, plus `envelope`). A wrapped capture with a `mapping` block also
describes how the native evidence maps into Gents runtime documents, and feeds
the native-capture roundtrip; one without it is validated as an envelope only.

| Generator | Runs | Writes |
| --- | --- | --- |
| `generators/langgraph/` | LangGraph `StateGraph` flows with an in-memory checkpointer, capturing `get_state_history` | `langgraph_state_history` captures: a linear retry/delegation graph, a compiled subgraph, a provider-shaped chat-model graph |
| `generators/autogen/` | AutoGen AgentChat teams with deterministic `BaseChatAgent` participants | `multi_agent_task` captures: `RoundRobinGroupChat` and a `Swarm` handoff chain |
| `generators/crewai/` | CrewAI sequential and hierarchical `Crew`s with deterministic `BaseLLM`s | `multi_agent_task` captures, including manager delegation through `delegate_work_to_coworker` |
| `generators/microsoft-agent-framework/` | A `GroupChatBuilder` workflow with deterministic `BaseChatClient`s | a `multi_agent_task` group-chat capture |

Each generator's `verify_export.py` checks the Gents export of its capture
against the native state, messages, participants, delegations and tool events.
The generators use no live inference, except that the LangGraph provider graph
calls an OpenAI-compatible endpoint when `GENTS_LANGGRAPH_PROVIDER_MODE` is
`live` (or `auto` with `OPENAI_API_KEY` set; `GENTS_LANGGRAPH_OPENAI_MODEL`
names the model). Its default, `fake`, uses `FakeListChatModel`.

## Running

`scripts/adapter-interop/run_docker_interop.sh [out-dir]` builds and runs every
generator, then runs both harnesses on the output. The output root defaults to
a directory under `/tmp` (or `GENTS_DOCKER_INTEROP_OUT`);
`GENTS_DOCKER_INTEROP_KEEP=1` keeps existing files and
`GENTS_DOCKER_INTEROP_SKIP_RUST=1` only generates. The `Adapter Interop`
workflow runs the same suite on `workflow_dispatch` or when a pull request gets
the `adapter-interop` label.

The harnesses are ignored tests:

```sh
GENTS_ADAPTER_INTEROP_FIXTURES=<dir> \
  cargo test -p gents-observe external_adapter_projection_fixtures_validate_against_contracts -- --ignored --nocapture
GENTS_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES=<dir> GENTS_ADAPTER_INTEROP_EXPORTS=<dir>/gents-exports \
  cargo test -p gents-cli --test integration cli_seeded::cli_adapter_interop_roundtrip::external_adapter_native_captures_project_to_export_formats -- --exact --ignored --nocapture
```

The first validates each fixture against the adapter projection DTO contract,
the generated envelope JSON Schema, and the generated adapter JSONL and
eval-JSONL records and their schemas. The second imports each capture that
has a `mapping` into a derived runtime view, checks the projection against it,
and writes the JSON, JSONL and eval-JSONL exports the `verify_export.py`
scripts read.
