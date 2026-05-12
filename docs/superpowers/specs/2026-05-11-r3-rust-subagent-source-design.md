# R3 Rust SubagentSource Design

## Scope

R3 is runtime plumbing on top of the R2 subagent data plane and the #161
tool-call closure work. It does not add Lean state-machine transitions. The
existing B-theorems cover the spawn edge, depth bound, linkage coherence, and
cascade behavior; this PR wires those invariants into Rust runtime dispatch.

## SubagentSource

`SubagentSource` is a `TriggerSource` sibling of `ScheduleSource`,
`EventSource`, and `ManualSource`. It subscribes to DefraDB `Update` events,
resolves the event collection id to a collection name, and filters for
`AgentToolCall` rows whose `child_request_id` is non-empty and whose
`lifecycle_state` is still `running`.

The source expects `spawn_subagent` tool-call args to contain:

```json
{
  "behavior_id": "target-behavior",
  "prompt": "child request prompt",
  "deadline": "optional RFC3339 deadline"
}
```

`target` and `target_behavior_id` are accepted as aliases for
`behavior_id`; `message` and `content` are accepted as aliases for `prompt`.
If both the tool-call row and args carry deadlines, the child request uses the
earlier deadline.

The tool-call row already carries the child request id allocated by the spawn
tool path. To preserve B5 link symmetry, the source materializes the child
`AgentRequest` using that exact request id. The existing
`create_subagent_request` helper remains available for callers that need a
fresh id; R3 adds an internal request-id-aware variant for source dispatch.

## TriggerEngine Handoff

`create_subagent_request` writes the `AgentRequest` before the engine sees the
fire. To avoid a second request materialization, `FireIntent` gains an
optional pre-materialized request id. When present, `TriggerEngine::dispatch`
returns `FireResult::Fired` for that id and skips enabled-gate, template,
concurrency, and materializer steps.

The persisted child request lineage is:

- `caused_by_trigger_kind = "subagent"`
- `caused_by_trigger_id = <parent_tool_call_id>`
- `caused_by_parent_request_id = <parent request_id>`
- `caused_by_parent_tool_call_id = <parent tool_call_id>`

The formal trigger vocabulary remains unchanged; `"subagent"` is runtime
lineage metadata for parent-linked child requests, not a new schedule/event
dispatch transition.

## Validation

R3 adds the deferred cross-reference checks from R2:

- `ToolSelection.subagent_targets[*]` must resolve to a behavior in the active
  document runtime view before the selection is applied.
- `create_subagent_request` rejects a missing parent `AgentRequest`, or a
  parent owned by a different `agent_did`, with
  `IllegalToolCallTransition::ParentLinkageIncoherent`.

The depth bound continues to use `MAX_SUBAGENT_DEPTH`, matching Lean's
`Subagent.maxSubagentDepth`.
