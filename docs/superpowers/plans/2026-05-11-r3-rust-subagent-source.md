# R3 Rust SubagentSource Plan

1. Add request-id-aware child request materialization while preserving the
   existing generated-id helper API.
2. Add parent existence validation to the shared child request creation path.
3. Add ToolSelection target cross-reference validation in document-view apply.
4. Add `SubagentSource` with DefraDB update subscription, AgentToolCall row
   hydration, args parsing, target lookup, depth inheritance, and duplicate
   spawn suppression.
5. Add a pre-materialized request id path to `FireIntent` and
   `TriggerEngine::dispatch`.
6. Wire `SubagentSource` into runtime startup after the startup barrier.
7. Add conformance coverage for source spawn, native no-op, missing parent,
   missing target, max depth, and cascade-through-source.
8. Run focused tests, formatting, `git diff --check`, and the relevant runtime
   and conformance suites before pushing the stacked PR.
