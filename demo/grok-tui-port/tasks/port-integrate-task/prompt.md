Run {{ event.correlation }} accepted sealed workspace
`{{ doc.workspace_id }}` for work unit `{{ doc.work_unit_id }}`
(implementation `{{ doc.implementation_id }}`).

This request is Integrate-bound. Do not git commit, git add, or mutate trunk.
Inspect the sealed tree if needed, then call `write_port_integrate_result`
once with unique `integrate_id`, `status=applied`, and a short `summary`.
Do not supply `run_id`, `work_unit_id`, `workspace_id`, or `expected_total`.
