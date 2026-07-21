# gents-schemas

Dependency-free GraphQL schema bundle for the gents agent collection
contract.

This crate owns the agent-domain `.graphql` files that document-peer consumers
need to share with gents:

- agent identity, behavior, runtime, and tool selection
- per-agent memory
- requests, rendered provider requests, responses, sessions, conversations,
  messages, tool calls, and tool results
- compaction, Codex thread projection, projection ACP bindings, tasks,
  schedules, event triggers, and peer pairing desired state

The crate intentionally has no runtime dependencies. Consumers that only need
the collection contract should depend on this crate instead of
`gents-protocol`.
