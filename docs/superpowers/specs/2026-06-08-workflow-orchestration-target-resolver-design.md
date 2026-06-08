# Workflow Orchestration and Target Resolution Design

Date: 2026-06-08
Status: design
Tracking issue: https://github.com/sourcenetwork/defra-agent/issues/378
Related:

- `docs/superpowers/specs/2026-06-03-true-subagent-enablement-design.md`
- `docs/superpowers/competitive-positioning/adapter-projection-template-roadmap.md`
- defending-code reference harness

## Summary

Defra Agent should add a workflow orchestration layer as runtime-enforced
primitives over the existing document spine, not as a static DAG executor that
must know every possible agent in the fleet.

The orchestrator is an `AgentBehavior`. It calls workflow tools such as
`fan_out_and_synthesize`, `pipeline`, `verify`, `loop_until_done`, and a
serial reducer/gate. The runtime resolves each requested target through the
orchestrator's authorized `ToolSelection.subagent_targets`, writes child
requests through the existing subagent bridge, and persists enough workflow
state to prove barriers, quorums, serial gates, budgets, and resume behavior.

The execution graph still materializes as Defra documents: `AgentRequest`,
`AgentToolCall`, child request lineage, `AgentResponse`, `AgentMessage`, and a
small set of workflow control documents. Local and remote targets use the same
model-facing name and the same lineage; locality is a runtime dispatch detail.

## Why this is not a normal DAG executor

A traditional DAG executor wants a complete graph and a complete worker catalog
before it starts. That is the wrong default for Defra Agent:

- some subagents are local and some are remote;
- remote `AgentBehavior` rows may not be replicated locally yet;
- target authorization is an operator-controlled permission surface, not a
  fleet-wide directory query;
- DefraDB ACP should ultimately decide which agent documents and capabilities a
  caller may read or use;
- useful agent workflows often discover the next branch at runtime.

The live runtime already encodes the right shape:

- `ToolSelection.subagent_targets` stores structured JSON targets with
  `name`, `agent_did`, `behavior_id`, and optional description.
- The model-facing `name` is the only identifier the agent passes to
  `spawn_subagent`.
- Local targets are checked against locally active behavior ids.
- Remote targets are retained only when
  `subagent_allow_cross_deployment == true` and are not required to resolve to
  local `AgentBehavior` rows.
- The spawn hook writes a normalized bridge payload with the resolved
  `(agent_did, behavior_id)` and lets `SubagentSource` materialize the child.

The workflow layer should build on this. It should resolve authorized targets
at primitive execution time, not require the DAG executor to own global
knowledge.

## Reference harness patterns

The defending-code reference harness gives a concrete set of Claude-style
workflows to port:

- interactive skill flows: quickstart, threat model, static vuln scan, triage,
  patch, customize;
- recon: produce focus areas before a large scan;
- parallel find: launch N independent find agents over focus areas or run
  indexes;
- grade: verify each submitted crash in a fresh sandbox with only PoC bytes
  crossing from find to grade;
- stream judge/dedupe: serialize a short no-tools judge over accepted graded
  crashes so the manifest has one writer;
- report: generate exploitability reports for new or better findings;
- report-grade: adversarially score report evidence;
- patch: generate a patch, apply/rebuild, replay PoC, run tests, then
  optionally re-attack;
- retry loops: feed failing evidence back to the patch agent up to a bounded
  retry count.

These are not one uniform DAG. They include fan-out, per-item pipelines,
adversarial verification, serial critical sections, artifact handoff, streaming
fan-in, and bounded retry loops. The Defra design should explicitly cover all
of those shapes.

## Product goal

Customers coming from LangGraph, CrewAI, OpenAI Agents SDK, Microsoft Agent
Framework, Claude dynamic workflows, or the reference harness should be able to
recognize their orchestration patterns in Defra Agent:

- durable graph state and checkpoints;
- role/specialist agents;
- handoffs and agents-as-tools;
- sequential and concurrent workflows;
- group or manager-led coordination;
- adversarial verification and human/automated gates;
- cross-deployment agent execution.

The differentiator is that Defra represents those workflows as permissioned
documents with DID-linked authorship, access control, lineage, and trace export
instead of framework-local state.

## Core design

### Orchestrator behavior

An orchestrator is a normal `AgentBehavior` with a `ToolSelection` that enables
workflow tools and subagent tools. It may be fired by:

- a `Task`;
- a `Schedule`;
- an `EventTrigger`;
- an A2A/ACP adapter-created `AgentRequest`;
- a direct CLI/API request.

The orchestrator decides what work to request. The runtime enforces what is
allowed and when the workflow can advance.

### Target resolver

Workflow primitives should accept target names or selectors, not raw behavior
ids:

```json
{
  "target": "canary-find",
  "prompt": "...",
  "await_mode": "background"
}
```

The resolver performs these steps for each target reference:

1. Load the parent request's current `ParentSubagentContext`.
2. Resolve `name` against the context's allowed `SubagentTarget` list.
3. Reject missing names, disabled spawning, disabled background mode, depth
   overflow, budget overflow, or remote targets without cross-deployment
   enablement.
4. Classify local vs remote by comparing target `agent_did` to the local DID.
5. For local targets, verify the behavior still exists before writing a child.
6. For remote targets, do not require a local `AgentBehavior`; rely on the
   resolved DID/behavior pair, P2P replication, and the remote claim path.
7. Return a resolved callable target:

```text
name
agent_did
behavior_id
locality: local | remote
description
await modes allowed
deadline/budget policy
```

This keeps the security boundary where it belongs: static target allowlist,
future DefraDB ACP, signed identities/capabilities, and runtime policy checks.
The LLM never gets to invent a target.

### Runtime-enforced primitives

The first primitive set should be:

1. `fan_out_and_synthesize`
   - Spawn N child requests.
   - Persist expected child ids.
   - Barrier until every required child is terminal or a configured failure
     policy fires.
   - Feed a deterministic result bundle to a synthesis step.

2. `pipeline`
   - For each item, run ordered stages independently.
   - No global barrier between items.
   - Each stage consumes the prior stage's artifact or response reference.

3. `verify`
   - Spawn independent verifier/refuter requests.
   - Enforce majority/quorum/threshold semantics in runtime code.
   - Persist the votes, threshold, and admitted/rejected result.

4. `loop_until_done`
   - Repeat a child workflow until a stop predicate, max rounds, deadline, or
     budget is reached.
   - The runtime enforces the loop budget; the model can only propose the next
     round.

5. `serial_reduce`
   - Serialize access to a shared reducer state.
   - This covers the reference harness judge/dedupe manifest, group-chat turn
     manager, and any "one writer decides canonical state" flow.
   - Without this primitive, concurrent findings can race and double-admit
     duplicates.

6. `artifact_handoff`
   - Pass references to persisted artifacts between steps.
   - Support constraints such as "only PoC bytes cross from find to grade" and
     "report receives source plus PoC, not find-agent reasoning."
   - This can start as a workflow metadata contract over existing files/docs,
     but should become a projection over Defra-owned artifact documents once
     artifact storage is first-class.

### Workflow documents

Existing lineage is enough to reconstruct a tree, but not enough to safely
resume or prove barriers, quorums, serial gates, and loop budgets. Add minimal
first-class workflow control documents.

`WorkflowRun`:

- `workflow_run_id`
- `root_request_id`
- `session_id`
- `orchestrator_agent_did`
- `orchestrator_behavior_id`
- `template_id`
- `status`: running, completed, failed, cancelled
- `budget_json`
- `created_at`, `completed_at`
- `metadata`

`WorkflowStep`:

- `workflow_step_id`
- `workflow_run_id`
- `parent_step_id`
- `primitive`
- `target_name`
- `resolved_agent_did`
- `resolved_behavior_id`
- `locality`
- `request_id`
- `tool_call_id`
- `child_request_id`
- `status`
- `input_ref_json`
- `output_ref_json`
- `created_at`, `completed_at`

`WorkflowBarrier`:

- `workflow_barrier_id`
- `workflow_run_id`
- `primitive`
- `expected_step_ids`
- `required_terminal_count`
- `observed_terminal_count`
- `failure_policy`
- `status`

`WorkflowReducer`:

- `workflow_reducer_id`
- `workflow_run_id`
- `name`
- `owner_step_id`
- `state_ref_json`
- `lock_state`
- `version`
- `updated_at`

These documents are control-plane state. The actual work remains
`AgentRequest`/`AgentToolCall`/`AgentResponse`. The workflow docs make the
control guarantees auditable and restartable.

## Framework pattern mapping

| Pattern | Defra template | Runtime primitive |
| --- | --- | --- |
| LangGraph durable graph | Nodes are tasks/behaviors; edges are persisted step outputs and event triggers. | `pipeline`, `loop_until_done`, `serial_reduce` |
| CrewAI Flow | Flow state document plus event/task transitions. | `pipeline`, `serial_reduce` |
| CrewAI Crew | Role-specialized behaviors under a manager. | `fan_out_and_synthesize`, `serial_reduce` |
| OpenAI handoff | Parent delegates ownership to a specialist. | subagent spawn plus run timeline projection |
| OpenAI agents-as-tools | Supervisor calls bounded specialists. | `fan_out_and_synthesize` or single target spawn |
| Microsoft sequential orchestration | Ordered behavior/task stages. | `pipeline` |
| Microsoft concurrent orchestration | Parallel children with configured fan-in. | `fan_out_and_synthesize` |
| Microsoft group chat/Magentic | Turn manager plus multiple specialized behaviors. | `serial_reduce`, `loop_until_done` |
| Claude dynamic workflows | Runtime-composed graph with deterministic primitives. | all primitives |
| Defending-code harness | Recon, N finds, grade, judge, report, patch, re-attack. | all primitives plus `artifact_handoff` |

## Defending-code harness template

A first serious proving template should be a Defra-native version of the canary
pipeline:

1. `recon` optionally produces focus areas.
2. `fan_out_and_synthesize` launches N `canary-find` workers.
3. `pipeline` sends each PoC artifact to a fresh `canary-grade` worker.
4. `serial_reduce` runs the judge/dedupe gate over accepted graded crashes.
5. `pipeline` launches report and report-grade for each new bug.
6. `loop_until_done` runs patch attempts with bounded T0/T1/T2/re-attack
   verification.

The first implementation slice does not need every stage. The minimum useful
proof is:

- build and run the canary target in Docker;
- run two or more find workers through Defra;
- grade each finding in a fresh container;
- assert only PoC bytes cross from find to grade;
- persist workflow run/step/barrier state;
- export the run timeline plus workflow projection.

This gives a real binary E2E test, not a mock-only workflow test.

## Testing strategy

### Unit and conformance tests

- Target resolver rejects unknown target names.
- Target resolver rejects remote targets when cross-deployment is disabled.
- Target resolver allows remote targets without requiring local behavior rows
  when cross-deployment is enabled.
- `fan_out_and_synthesize` cannot synthesize before every required child is
  terminal.
- `pipeline` advances one item without waiting for unrelated items.
- `verify` admits only threshold-satisfying results.
- `loop_until_done` stops at predicate, deadline, or budget.
- `serial_reduce` preserves one-writer reducer state.

### Embedded-node E2E

- Single-node fan-out over two local subagents.
- Background children materialize with correct parent request/tool-call
  lineage.
- Barrier survives runtime restart and resumes from persisted state.
- Cancellation cascades from workflow run to live children.
- Run timeline projection includes workflow steps and child lineage.

### Simulated-fleet E2E

- Two-node local/remote parity: one local target and one remote target in the
  same fan-out.
- Remote target is not required to resolve locally.
- Child terminal propagation completes the same barrier as local children.
- Cross-deployment timeout and unclaimed-spawn failure appear as workflow step
  failures.

### Real binary E2E

- Use the defending-code canary target and Docker harness assets.
- Run Defra Agent as the in-container backend.
- Exercise live inference when enabled by environment.
- Verify generated result files, PoC bytes, grade verdicts, workflow docs, and
  run timeline output.

### Live inference gate

Keep non-live tests deterministic, but add an opt-in live gate for the harness
template:

```text
DEFRA_AGENT_INFERENCE_URL
DEFRA_AGENT_API_KEY or DEFRA_AGENT_API_KEY_ENV_VAR
VULN_PIPELINE_MODEL
DEFRA_AGENT_BINARY
```

The gate should fail loudly on setup errors and skip cleanly when the required
environment is absent.

## Formal-methods plan

Add `Proofs/Workflow/` only once the first persisted workflow documents are
chosen. The proof targets should compose with `Proofs/Background/` rather than
re-model subagent execution.

Properties:

- barrier completeness: synthesis is reachable only after all required children
  are terminal or after an explicit failure policy;
- no hidden global barrier: pipeline item advancement depends only on that
  item's prior stage;
- quorum soundness: admitted verifier result implies the threshold was met over
  the persisted votes;
- loop boundedness: every loop is limited by stop predicate, max rounds,
  deadline, or budget;
- serial reducer exclusivity: at most one reducer update owns a reducer version;
- authorization invariant: every workflow-spawned child has a target that was
  resolved from the parent's allowed targets;
- local/remote equivalence: once a child request is materialized, barrier and
  projection behavior is independent of target locality.

New collections mean the apply/reconcile collection parity contract must be
updated alongside schema additions.

## Implementation slices

1. Design and schema slice
   - Land this design.
   - Add `WorkflowRun`, `WorkflowStep`, `WorkflowBarrier`, and
     `WorkflowReducer` schemas.
   - Add desired-state/export names and collection parity coverage.

2. Target resolver library
   - Extract target resolution from subagent spawn into a reusable runtime
     resolver.
   - Keep the existing `spawn_subagent` behavior unchanged.
   - Add tests for local/remote, disabled cross-deployment, stale local
     behavior, and malformed target entries.

3. Fan-out barrier primitive
   - Implement `fan_out_and_synthesize` as the first workflow tool.
   - Persist workflow run, step, and barrier docs.
   - Prove synthesis cannot run early.
   - E2E on a live embedded node with two local child behaviors.

4. Local/remote fan-out E2E
   - Use the two-node test harness.
   - Fan out to one local and one remote target.
   - Assert identical barrier semantics after child materialization.

5. Reference harness canary template
   - Port canary find plus grade into a Defra workflow template.
   - Use Docker for real binary execution.
   - Add opt-in live inference E2E.

6. Serial reducer and verifier slice
   - Add `serial_reduce` and `verify`.
   - Port the judge/dedupe and report-grade gates from the reference harness.

7. Pipeline and loop slice
   - Add `pipeline` and `loop_until_done`.
   - Port report/patch/re-attack as bounded workflow templates.

8. Projections and exports
   - Extend run timeline projection with workflow run/step/barrier events.
   - Export workflow traces for adapter views and training-data extraction.

## Open questions

- Should workflow tools be enabled by existing `subagent_spawn_enabled`, or does
  `ToolSelection` need a separate `workflow_orchestration_enabled` field?
- Are workflow templates first-class desired-state documents, or are they
  manifest bundles made of tasks, behaviors, and tool selections until runtime
  proves the need for a `WorkflowTemplate` schema?
- Where should artifact references live before a first-class artifact document
  exists?
- Should `serial_reduce` use a dedicated reducer document or a generic
  compare-and-swap helper over any authorized Defra document?
- Which live inference target should be the default for CI-adjacent opt-in
  testing?

## Recommendation

Proceed with the schema and resolver slices first, then implement
`fan_out_and_synthesize` against a live embedded node. Do not start with a
general DAG language. The primitive set plus Defra-native workflow documents is
enough to port the important external framework patterns, and it preserves the
product thesis: the durable, permissioned document graph is the control plane.
