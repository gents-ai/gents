# Microsoft Agent Framework Fixture Generator

This generator executes a real Microsoft Agent Framework group-chat workflow
with deterministic custom `BaseChatClient` instances. It captures native
workflow and group-chat events and writes a wrapped Gents
`multi_agent_task` adapter projection fixture.

It emits:

- `multi_agent_task.microsoft_agent_framework_group_chat.capture.json`: a
  `GroupChatBuilder` workflow with researcher and writer agents selected by a
  round-robin group-chat orchestrator. The fixture projects centralized
  orchestration, shared conversation flow, participant turns, request
  boundaries, and completion.

Build and run it from the repository root:

```sh
docker build \
  -t defra-agent-msaf-fixture \
  scripts/adapter-interop/generators/microsoft-agent-framework

rm -rf /tmp/defra-agent-msaf-fixtures
mkdir -p /tmp/defra-agent-msaf-fixtures

docker run --rm \
  -v /tmp/defra-agent-msaf-fixtures:/out \
  defra-agent-msaf-fixture
```

Validate the generated fixture with the shared external adapter harness:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=/tmp/defra-agent-msaf-fixtures \
  cargo test -p defra-agent --test e2e_runtime adapter_projection_external_fixtures -- --ignored --nocapture
```

The generator avoids live inference by using custom deterministic chat clients
and installs only `agent-framework-core` plus `agent-framework-orchestrations`,
but it still exercises Microsoft Agent Framework's `Agent`, `BaseChatClient`,
`GroupChatBuilder`, `GroupChatState`, workflow streaming, group-chat request and
response events, and response content surfaces.
