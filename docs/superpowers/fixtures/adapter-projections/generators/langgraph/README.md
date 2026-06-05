# LangGraph Fixture Generator

This generator executes a real LangGraph `StateGraph` with an in-memory
checkpointer, captures `graph.get_state_history(config)`, and writes a wrapped
Defra Agent `langgraph_state_history` adapter projection fixture.

Build and run it from the repository root:

```sh
docker build \
  -t defra-agent-langgraph-fixture \
  docs/superpowers/fixtures/adapter-projections/generators/langgraph

rm -rf /tmp/defra-agent-langgraph-fixtures
mkdir -p /tmp/defra-agent-langgraph-fixtures

docker run --rm \
  -v /tmp/defra-agent-langgraph-fixtures:/out \
  defra-agent-langgraph-fixture
```

Validate the generated fixture with the shared external adapter harness:

```sh
DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES=/tmp/defra-agent-langgraph-fixtures \
  cargo test -p defra-agent --test adapter_projection_external_fixtures -- --ignored --nocapture
```

The normal Rust test suite does not run Docker. This generator is an
interop-proof path for checking the adapter contract against a real upstream
LangGraph state-history capture.
