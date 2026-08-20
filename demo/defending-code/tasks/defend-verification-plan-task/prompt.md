Defense run {{ group.correlation_value }} has {{ group.count }} completed scan
documents (complete={{ group.complete }}):

{{ group.docs }}

Call `read_defense_candidate` exactly once. Let N be the exact number of
returned candidates. In stable `finding_id` order call
`write_defense_verification_assignment` exactly N times, once per candidate,
with `assignment_id=<finding_id>:verify`, the candidate's exact `finding_id`
and `area_id`, `status=ready`, and `expected_total=N`.

If N is zero, call the write tool exactly once with
`assignment_id={{ group.correlation_value }}:no-candidates`,
`finding_id=none`, `area_id=none`, `status=skipped`, and `expected_total=1`.
Do not launch or wait for agents. Do not supply runtime-filled `run_id` or
`repository_path`.
