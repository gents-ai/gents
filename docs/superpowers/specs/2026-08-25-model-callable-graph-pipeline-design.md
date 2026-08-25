# Model-callable graph compilation over existing automation

**Status:** experimental stacked implementation.

**Supersedes:** the direct model-to-Task/EventTrigger compiler in PR #1176.

## Decision

Ship a small compiler and publication adapter, not a graph runtime.

A model submits topology over operator-approved `StageCapability` revisions.
Each capability wraps an existing Task and declares its typed ports. The pure
compiler validates the complete DAG and resolves edges to existing task IDs and
physical collections. One identity-scoped transaction then writes ordinary
EventTriggers plus an immutable GraphDefinition audit record.

```text
model GraphIntent
      │
      ▼
pure validation against approved existing Tasks
      │
      ▼
one transaction: GraphDefinition + EventTriggers
      │
      ▼
existing reconciliation → trigger engine → Task runtime
```

The endpoint never accepts an executable `GraphPlan` from the model. It never
creates Tasks or accepts behavior IDs, prompts, tool selections, models, or
physical collections as authority-bearing model input.

Execution is deliberately separate. After normal reconciliation, an existing
bounded write tool creates an entry document. The existing trigger/task runtime
does everything after that point.

## Why this is smaller and safer

The repository already owns execution, recovery, identity, tool resolution,
lineage, event fan-out/fan-in, and persistence. A revision controller and a new
GraphRun lifecycle duplicated those responsibilities without actually pinning
live execution.

Independent reviews found that the larger design:

- allowed a model to forge a self-hashed GraphPlan and bypass capability checks;
- seeded entries before trigger reconciliation, losing the first event;
- treated mutable Task/EventTrigger rows as immutable digest-addressed artifacts;
- retired triggers still needed by supposedly pinned runs;
- advertised invocation and tool-selection pinning that execution ignored;
- added node-wide watcher queries and full reloads to every control update.

Removing that layer fixes the defects by removing the false abstraction. The
only new persisted type is an immutable GraphDefinition containing the accepted
plan digest and canonical plan JSON. It is audit metadata, not a runtime gate.

## Trust boundary

### Operator-owned capability

`StageCapability` contains:

- stable capability ID and revision;
- an existing Task ID;
- typed input/output ports, including physical collection and correlation field;
- caller DIDs allowed to compose it.

The Task remains the single source of its behavior, prompt, tools, model, and
output permissions. Editing those existing documents has the same semantics it
does elsewhere in Gents; v1 makes no runtime-pinning claim.

### Model-owned intent

`GraphIntent` contains only graph/node IDs, capability selections, port edges,
entry bindings, supported delivery modes, safe predicates, and structural
bounds. The model does not supply resolved Task IDs or collections.

### Identity-scoped publication

`CompileGraphTool` owns one concrete `Did`; the same DID is used for capability
authorization and every statement in `ConfigApplyTxn`. There is no independent
caller string and no `identity=None` path. Operators may put `compile_graph` in
the existing `approval_required_tools` list when mutation needs human approval.

## Compiler contract

The compiler rejects the full intent without writes unless all of these hold:

1. IDs are unique and all node/port references resolve.
2. Every node selects an allowed capability revision.
3. Required inputs have exactly one entry or inbound edge.
4. Connected collection/schema/correlation/cardinality contracts match.
5. Collections and correlation fields pass shared GraphQL validators.
6. Predicates pass the existing safe filter-fragment validator.
7. Every node is reachable from an entry and the graph is acyclic.
8. Node, edge, depth, fan-out, and group-size bounds hold.
9. Two graph nodes do not claim the same output collection.

Rule 9 reflects the actual EventTrigger abstraction: routing is collection-wide,
not producer-node-specific. Rejecting shared output collections keeps an edge's
`from.node_id` meaningful instead of pretending the runtime can distinguish
producers it cannot observe.

Successful output is canonical and digest-stable. The tool compiles the intent
internally, and the publisher verifies the resulting plan digest immediately
before writing. The plan never crosses the model boundary, so the digest is
content identity rather than proof supplied by an untrusted caller.

## Publication contract

Publication verifies that every planned Task currently exists and is enabled,
then writes all entry/edge EventTriggers and the GraphDefinition in one
transaction. The existing config writers retain their convergence, GraphQL
escaping, empty-list, and conflict behavior.

Graph IDs are immutable in v1. Repeating the same plan is idempotent; a changed
plan must use a new graph ID. This avoids introducing revision activation,
cleanup, and in-flight migration semantics before there is evidence they are
needed.

The tool response means the configuration committed, not that every host has
reconciled it. Entry writes remain a later operation, matching all existing
document-driven automation.

## Explicit non-goals

- creating or editing Tasks, prompts, behaviors, tools, models, or schemas;
- starting runs or defining a second run lifecycle;
- active-revision pointers, hot upgrades, or in-flight revision pinning;
- enforcing runtime invocation budgets;
- distinguishing multiple producers that write the same collection;
- production configuration wiring before the experiment graduates.

## Evaluation gate

The checked-in fixtures cover deterministic acceptance and repair diagnostics.
Graduation additionally requires a live evaluation that:

1. constructs the custom tool with an operator-owned capability catalog;
2. compiles linear, fan-out, routing, and fan-in graphs;
3. waits for ordinary runtime reconciliation;
4. writes entries through existing bounded write tools;
5. observes requests, outputs, restart behavior, and authorization failures.

Measure repair turns, token/latency cost, digest stability, publication errors,
reconciliation latency, and end-to-end task correctness.

## Formal boundary

`Proofs.GraphPipeline` proves that publication requires the conjunction of type,
topology, capability-authorization, and bound checks, that invalid graphs cannot
publish, and that the publication transition preserves safety. Lean emits the
complete four-bit validation matrix consumed by Rust conformance tests. DefraDB
transaction serializability and runtime reconciliation remain declared platform
boundaries.

## Stack

1. research, narrowed design, and Lean publication boundary;
2. pure typed compiler and generated conformance cases;
3. transactional GraphDefinition/EventTrigger publication over existing Tasks;
4. one model-facing `compile_graph` custom tool and evaluation fixtures.

## References

- [AFlow: Automating Agentic Workflow Generation](https://arxiv.org/abs/2410.10762)
- [Automated Design of Agentic Systems](https://arxiv.org/abs/2408.08435)
- [GPTSwarm: Language Agents as Optimizable Graphs](https://arxiv.org/abs/2402.16823)
- [CaMeL: Defeating Prompt Injections by Design](https://arxiv.org/abs/2503.18813)
