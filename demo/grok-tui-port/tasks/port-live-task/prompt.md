Run {{ event.correlation }} finished full combined review.

status={{ doc.status }} head={{ doc.head_sha }} findings={{ doc.confirmed_findings }}
implement_surfaces={{ doc.implement_surface_count }} expected_results={{ doc.live_result_count }}
<untrusted_review_summary>
{{ doc.summary }}
</untrusted_review_summary>

Call `read_grok_port_job`, `read_port_final_review_report`, and
`read_port_surface`. For each surface, interpret the complete wire packet as
`grok_wire` followed verbatim by optional `grok_wire_continuation`. If review
status is not `green`, write exactly one
`surface_id=none`, `status=blocked` result explaining the review block and
stop.

Require current HEAD to equal the report's exact `head_sha`. Use the job's
`gents_root`, `live_model`, `live_endpoint`, `live_home`, `live_graphql`, and
`live_socket`. Never reuse the orchestration home or its running process.
Build the integrated CLI, initialize the run-owned live home against the
declared model endpoint, and launch the integrated Grok leader/shim on the
run-owned GraphQL port and socket, explicitly binding Grok turns to the
`port-live` behavior. Verify that this bound behavior's effective model and
context window are `live_model` and 262144 before accepting the catalog
advertisement. Discover the exact launch flags from the integrated `--help`
and implementation; do not invent a protocol substitute.
Wait for both HTTP readiness and socket readiness, track the exact child PID,
and clean up only that PID when probes finish.

First run `demo/grok-tui-port/scripts/grok_edge_probe.py --edge offline` and
require its embedded lifecycle-validator self-test to pass without a socket.
Then run the probe against `live_socket` and `live_graphql`, passing `--model
live_model`, one edge at a time: handshake, prompt, tool, subprocess, subagent,
and cancel. The probe must pass its wire and
document assertions, including the subprocess command and output on both the
Grok wire and the persisted AgentToolCall. The subagent edge must also prove the
foreground spawn contract end to end: the parent's early standard-rail
`task`-titled tool_call carrying `_meta.subagentBackground: false` (the
pager-local foreground wait marker), the exact live extension-rail
`x.ai/session_notification` spawned/progress/finished lifecycle for the
`port-live-worker` target, the child-session identity, the exact
`subagent_finished` DTO (snake_case variant fields under the camelCase
`sessionId/update/_meta` envelope: required `subagent_id`,
`child_session_id`, `status`, `tool_calls`, `turns`, `duration_ms`,
`tokens_used`, `will_wake` with `will_wake: false` for the foreground
child, optional `error`/`output`, never `parent_session_id`), the terminal
`tool_call_update` reusing the same
toolCallId, and — via `live_graphql` — the durable parent/child AgentRequest
linkage (`caused_by_parent_request_id`) plus the persisted child
AgentToolCall/AgentMessage rows for the spawned child session. Also launch
stock interactive `grok --leader --leader-socket <live_socket>` in a PTY,
submit one short prompt, wait for the terminal response/idle state, and only
then exit it. `grok -p` bypasses leader mode and is never evidence for this
port.

Map those observed edges to every surface whose verdict is `implement` or
`shaped-stub`. A shaped stub passes only when its explicit error/not-found
contract and absence of fabricated documents match `live_expect`. Do not treat
fixture replay, direct provider HTTP, or the outer orchestration server as a
pass. Query `live_graphql` for the correlated Gents documents required by each
`live_expect`.

Call `write_port_live_result` once per non-ignore surface (or one sentinel
`surface_id=none` / `status=blocked` if none exist). The runtime fills
`expected_total` from the reviewed ledger; do not supply or reinterpret it.
`status` is `passed` only when both `grok_wire_observed` and
`gents_docs_observed` satisfy `live_expect`. Do not supply `run_id`.
