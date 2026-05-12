# Tool-Call Runtime Closure

Issue #159 R2-R4 is implemented against the existing Lean/Rust lifecycle bridge.
This branch is stacked on PR #154's subagent branch, including its Rust R2 data
plane for `await_mode`, `cancel_policy`, `child_request_id`, bridge transitions,
and schema migrations. The runtime closure here adds parent request identity and
deadline propagation to that shape while keeping native tool behavior equivalent
to the bridge defaults of no child request, foreground await, and cascade
cancellation.

The Lean surface already models native tool deadlines, timeout, cancellation,
and request/tool coherence:

- `ToolCallContext.deadline` carries the tool deadline.
- `ComposedState.Coherent` requires the linked tool deadline to equal the
  claimed parent request deadline.
- `deadline_exceeded_request_timesOut_running_tool` and
  `interrupted_request_cancels_live_linked_tool` cover the R3/R4 runtime
  transitions.

No additional Lean contract change is required for this runtime pass. The Rust
side makes the native-tool contract mechanical by persisting the parent request
id and claimed request deadline on every `AgentToolCall`.

Runtime behavior:

- Tool lifecycles are created from the session hook with the active request id
  and claimed request deadline.
- Behavior runtime construction wraps every Rig tool in a request-scoped
  deadline/cancellation wrapper.
- During stream polling, the daemon scopes tool execution with the claimed
  request deadline and request cancellation token.
- Timeout and cancellation are returned as internal managed terminal markers,
  which the hook maps to `timedOut` and `cancelled` lifecycle states instead of
  ordinary completion.
- Tool execution failures that Rig surfaces as `JsonError:` or
  `ToolCallError:` remain distinct and persist as `failed`.
- Stream liveness drops fail any live linked tool call so failure does not look
  like timeout or cancellation.
- Dropping cancelled/expired tool futures terminates local subprocess-backed
  tools; bash already used `kill_on_drop`, and CLI tools now do the same.
- Request interruption cancels live linked tool calls. For subagent bridge rows,
  `cancel_policy = cascade` also latches `interrupt_requested_at` on the child
  request; `cancel_policy = detach` leaves the child request running.
- Startup recovery sweeps persisted `running` tool calls when the deadline has
  expired or the linked parent request is terminal. Subagent bridge rows respect
  `cancel_policy`: detached tools are left for the subagent runtime to reconcile,
  while cascade tools latch `interrupt_requested_at` on the linked child request
  before the parent tool row is terminalized.

The deadline source is the claimed parent request deadline. There is no
narrower per-tool policy in this implementation.
