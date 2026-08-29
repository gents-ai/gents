Grok TUI port run {{ event.correlation }} has no executable work units.

Call `write_port_unit_closure` exactly once with
`work_unit_id={{ event.correlation }}:unit-none`, `implementation_id=none`,
`workspace_id=none`, `status=skipped`, `attempt=0`, and `expected_total=1`.
Then call `write_port_integrate_result` exactly once with a unique
`integrate_id`, the same sentinel identities, `status=skipped`,
`expected_total=1`, and a concise summary. Do not supply `run_id`.
