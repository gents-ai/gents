# LangGraph Fixture Generator

This generator executes real LangGraph `StateGraph` flows with an in-memory
checkpointer, captures `graph.get_state_history(config)`, and writes wrapped
Defra Agent `langgraph_state_history` adapter projection fixtures.

It emits:

- `langgraph_state_history.capture.json`: a linear graph with retry and child
  request evidence.
- `langgraph_state_history.subgraph.capture.json`: a parent graph that runs a
  compiled review subgraph and projects the nested nodes, transitions, tasks,
  and child request boundary.
- `langgraph_state_history.provider.capture.json`: a provider-shaped graph that
  invokes a LangChain chat model, checkpoints `HumanMessage`, `AIMessage`, and
  `ToolMessage` objects, and projects the model/tool boundary as a child task.

Build and run it from the repository root:

```sh
docker build \
  -t defra-agent-langgraph-fixture \
  scripts/adapter-interop/generators/langgraph

rm -rf /tmp/defra-agent-langgraph-fixtures
mkdir -p /tmp/defra-agent-langgraph-fixtures

docker run --rm \
  -v /tmp/defra-agent-langgraph-fixtures:/out \
  defra-agent-langgraph-fixture
```

By default, the provider fixture uses LangChain's deterministic
`FakeListChatModel` so it can run in CI or local Docker without credentials.
To exercise a live OpenAI-compatible endpoint, pass:

```sh
docker run --rm \
  -e DEFRA_LANGGRAPH_PROVIDER_MODE=live \
  -e OPENAI_API_KEY \
  -e OPENAI_BASE_URL \
  -e DEFRA_LANGGRAPH_OPENAI_MODEL=gpt-4.1-mini \
  -v /tmp/defra-agent-langgraph-fixtures:/out \
  defra-agent-langgraph-fixture
```

`DEFRA_LANGGRAPH_PROVIDER_MODE=auto` uses the live endpoint when
`OPENAI_API_KEY` is set and otherwise falls back to the deterministic fake
model. `live` fails if no API key is present.

Validate the generated fixture with the shared external adapter harness:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=/tmp/defra-agent-langgraph-fixtures \
  cargo test -p defra-agent --test e2e_runtime adapter_projection_external_fixtures -- --ignored --nocapture
```

The normal Rust test suite does not run Docker. This generator is an
interop-proof path for checking the adapter contract against real upstream
LangGraph state-history captures, including compiled-subgraph and
provider-backed chat-message shapes.
