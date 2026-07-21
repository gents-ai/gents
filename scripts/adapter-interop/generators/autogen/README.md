# AutoGen Fixture Generator

This generator executes real AutoGen AgentChat teams with deterministic custom
`BaseChatAgent` participants. It captures native `TaskResult` values and writes
wrapped Gents `multi_agent_task` adapter projection fixtures.

It emits:

- `multi_agent_task.autogen.capture.json`: a `RoundRobinGroupChat` team with
  planner, researcher, and reviewer turns.
- `multi_agent_task.autogen_swarm.capture.json`: a `Swarm` team that routes
  planner -> researcher -> reviewer through native `HandoffMessage` events and
  projects the resulting delegation chain.

Build and run it from the repository root:

```sh
docker build \
  -t gents-autogen-fixture \
  scripts/adapter-interop/generators/autogen

rm -rf /tmp/gents-autogen-fixtures
mkdir -p /tmp/gents-autogen-fixtures

docker run --rm \
  -v /tmp/gents-autogen-fixtures:/out \
  gents-autogen-fixture
```

Validate the generated fixture with the shared external adapter harness:

```sh
GENTS_ADAPTER_INTEROP_FIXTURES=/tmp/gents-autogen-fixtures \
  cargo test -p gents --test e2e_runtime adapter_projection_external_fixtures -- --ignored --nocapture
```

The generator avoids live inference by using custom deterministic agents, but
it still exercises AutoGen AgentChat's team runtime, message flow, native
handoff routing, and termination surface.
