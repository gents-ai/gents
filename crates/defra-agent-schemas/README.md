# defra-agent-schemas

Dependency-free GraphQL schema bundle for the defra-agent agent collection
contract.

This crate owns the agent-domain `.graphql` files that document-peer consumers
need to share with defra-agent:

- agent identity, behavior, runtime, and tool selection
- requests, responses, sessions, conversations, messages, tool calls, and tool
  results
- compaction, Codex thread projection, tasks, schedules, event triggers, and
  peer pairing desired state

The crate intentionally has no runtime dependencies. Consumers that only need
the collection contract should depend on this crate instead of
`defra-agent-protocol`.
