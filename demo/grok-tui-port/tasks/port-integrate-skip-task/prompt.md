Run {{ event.correlation }} blocked work unit `{{ doc.work_unit_id }}`
after attempt {{ doc.attempt }} (implementation `{{ doc.implementation_id }}`).

Call `write_port_integrate_result` once with unique `integrate_id`,
`status=skipped`, and a summary of the block. Do not supply `run_id`,
`work_unit_id`, `workspace_id`, or `expected_total`.
