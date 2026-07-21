# CrewAI Fixture Generator

This generator executes real CrewAI `Crew` workflows with deterministic custom
`BaseLLM` instances. It captures native CrewAI `CrewOutput` / task output
objects and writes wrapped Gents `multi_agent_task` adapter projection
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
  -t gents-crewai-fixture \
  scripts/adapter-interop/generators/crewai

rm -rf /tmp/gents-crewai-fixtures
mkdir -p /tmp/gents-crewai-fixtures

docker run --rm \
  -v /tmp/gents-crewai-fixtures:/out \
  gents-crewai-fixture
```

Validate the generated fixture with the shared external adapter harness:

```sh
GENTS_ADAPTER_INTEROP_FIXTURES=/tmp/gents-crewai-fixtures \
  cargo test -p gents --test e2e_runtime adapter_projection_external_fixtures -- --ignored --nocapture
```

The generator avoids live inference by using custom deterministic LLMs, but it
still exercises CrewAI's `Agent`, `Task`, `Crew`, `Process.sequential`,
`Process.hierarchical`, `BaseLLM`, manager delegation tools, task-context,
kickoff, and output surfaces.
