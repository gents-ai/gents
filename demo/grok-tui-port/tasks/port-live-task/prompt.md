Run {{ event.correlation }} finished full combined review.

status={{ doc.status }} head={{ doc.head_sha }} findings={{ doc.confirmed_findings }}
implement_surfaces={{ doc.implement_surface_count }} expected_results={{ doc.live_result_count }}
<untrusted_review_summary>
{{ doc.summary }}
</untrusted_review_summary>

Call `read_grok_port_job`, `read_port_final_review_report`, and
`read_port_surface`. If review status is not `green`, write exactly one
`surface_id=none`, `status=blocked` result explaining the review block and
stop.

Require current HEAD to equal the report's exact `head_sha`. Use the job's
`gents_root`, `live_model`, `live_endpoint`, `live_home`, `live_graphql`, and
`live_socket`. Never reuse the orchestration home or its running process.
Build the integrated CLI, initialize the run-owned live home against the
declared model endpoint, and launch the integrated Grok leader/shim on the
run-owned GraphQL port and socket. Discover the exact launch flags from the
integrated `--help` and implementation; do not invent a protocol substitute.
Wait for both HTTP readiness and socket readiness, track the exact child PID,
and clean up only that PID when probes finish.

For every surface with
`verdict=implement`, send that row's `live_prompt` as a real user turn
through stock `grok --leader --leader-socket <live_socket>` against the new
integrated process. Do not treat fixture replay, direct provider HTTP, or the
outer orchestration server as a pass. Query `live_graphql` for the correlated
Gents documents required by `live_expect`.

Call `write_port_live_result` once per implement surface (or one sentinel
`surface_id=none` / `status=blocked` if none exist). The runtime fills
`expected_total` from the reviewed ledger; do not supply or reinterpret it.
`status` is `passed` only when both `grok_wire_observed` and
`gents_docs_observed` satisfy `live_expect`. Do not supply `run_id`.
