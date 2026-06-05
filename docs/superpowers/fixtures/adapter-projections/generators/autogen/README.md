# AutoGen Fixture Generator

This generator executes a real AutoGen AgentChat `RoundRobinGroupChat` with
deterministic custom `BaseChatAgent` participants. It captures the native
`TaskResult` and writes a wrapped Defra Agent `multi_agent_task` adapter
projection fixture.

Build and run it from the repository root:

```sh
docker build \
  -t defra-agent-autogen-fixture \
  docs/superpowers/fixtures/adapter-projections/generators/autogen

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
it still exercises AutoGen AgentChat's team runtime, message flow, and
termination surface.
