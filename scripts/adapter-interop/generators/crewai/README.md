# CrewAI Fixture Generator

This generator executes real CrewAI `Crew` workflows with deterministic custom
`BaseLLM` instances. It captures native CrewAI `CrewOutput` / task output
objects and writes wrapped Defra Agent `multi_agent_task` adapter projection
fixtures.

It emits:

- `multi_agent_task.crewai_sequential.capture.json`: a sequential CrewAI crew
  with planner, researcher, and reviewer tasks. The fixture projects task
  assignment, context flow, review, and parent/child request boundaries.
- `multi_agent_task.crewai_hierarchical.capture.json`: a hierarchical CrewAI
  crew where a manager delegates research and review tasks to coworkers through
  CrewAI's `delegate_work_to_coworker` tool. The fixture projects repeated
  manager-to-worker child request boundaries and task context from research
  into review.

Build and run it from the repository root:

```sh
docker build \
  -t defra-agent-crewai-fixture \
  scripts/adapter-interop/generators/crewai

rm -rf /tmp/defra-agent-crewai-fixtures
mkdir -p /tmp/defra-agent-crewai-fixtures

docker run --rm \
  -v /tmp/defra-agent-crewai-fixtures:/out \
  defra-agent-crewai-fixture
```

Validate the generated fixture with the shared external adapter harness:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=/tmp/defra-agent-crewai-fixtures \
  cargo test -p defra-agent --test adapter_projection_external_fixtures -- --ignored --nocapture
```

The generator avoids live inference by using custom deterministic LLMs, but it
still exercises CrewAI's `Agent`, `Task`, `Crew`, `Process.sequential`,
`Process.hierarchical`, `BaseLLM`, manager delegation tools, task-context,
kickoff, and output surfaces.
