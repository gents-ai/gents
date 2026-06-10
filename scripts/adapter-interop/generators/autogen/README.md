# AutoGen Fixture Generator

This generator executes real AutoGen AgentChat teams with deterministic custom
`BaseChatAgent` participants. It captures native `TaskResult` values and writes
wrapped Defra Agent `multi_agent_task` adapter projection fixtures.

It emits:

- `multi_agent_task.autogen.capture.json`: a `RoundRobinGroupChat` team with
  planner, researcher, and reviewer turns.
- `multi_agent_task.autogen_swarm.capture.json`: a `Swarm` team that routes
  planner -> researcher -> reviewer through native `HandoffMessage` events and
  projects the resulting delegation chain.

Build and run it from the repository root:

```sh
docker build \
  -t defra-agent-autogen-fixture \
  scripts/adapter-interop/generators/autogen

rm -rf /tmp/defra-agent-autogen-fixtures
mkdir -p /tmp/defra-agent-autogen-fixtures

docker run --rm \
  -v /tmp/defra-agent-autogen-fixtures:/out \
  defra-agent-autogen-fixture
```

Validate the generated fixture with the shared external adapter harness:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=/tmp/defra-agent-autogen-fixtures \
  cargo test -p defra-agent --test adapter_projection_external_fixtures -- --ignored --nocapture
```

The generator avoids live inference by using custom deterministic agents, but
it still exercises AutoGen AgentChat's team runtime, message flow, native
handoff routing, and termination surface.
