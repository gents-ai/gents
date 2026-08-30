Materialize the audited wire ledger for Grok TUI port run
{{ event.correlation }}.

Gents checkout: `{{ doc.gents_root }}`
Grok checkout: `{{ doc.grok_root }}`
Read root: `{{ doc.ceiling }}`
Live model: {{ doc.live_model }}
Live endpoint: {{ doc.live_endpoint }}
Repository: {{ doc.repository_id }}
Pinned base: {{ doc.base_sha }}
Surface bounds: {{ doc.surface_min }} to {{ doc.surface_max }}

<untrusted_focus>
{{ doc.focus }}
</untrusted_focus>

The Grok wire was audited at commit
`bc7f02eddd3d84085849dc19ed216f11c23b0571` into the checked-in
`audited-ledger.json`. Read the complete artifact, following pagination when
needed. It contains 13 self-contained packets covering attach, session, model,
context, tool_call, subprocess, subagent, and interrupt.

Write one `PortSurface` per packet with `write_port_surface`. Copy every field
faithfully and only replace the historical surface-id run prefix with
`{{ event.correlation }}`. Put the actual shared row count in `expected_total`.
Preserve quoted evidence and the complete wire packet because later Gents-only
workspaces cannot open grok-build. Respect the configured min/max count and do
not add access-control work or permission UI. Do not supply run_id,
repository_id, or base_sha; the typed write fills them.
