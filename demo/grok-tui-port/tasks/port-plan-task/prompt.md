Grok TUI port run {{ event.correlation }} finished reconnaissance audit:

<untrusted_audit>
status={{ doc.status }} count={{ doc.surface_count }}
{{ doc.summary }}
{{ doc.missing_areas }}
</untrusted_audit>

Call `read_port_recon_audit` and `read_port_surface` for the full ledger.
If the audit status is not `accepted`, write no executable unit. Instead write
one non-executable `PortWorkUnit` sentinel with `work_unit_id` ending in
`:unit-none`, `status=skipped`, `surface_ids=none`, `verdict=ignore`,
`attempt=0`, `branch=none`, `expected_total=1`, and concise values for every
other required field. Then close the plan with all executable counts zero.

For an accepted audit, cluster only `implement` and
`shaped-stub` rows into work units. Do not cluster by Codex filenames. Do
not mix `implement` and `shaped-stub` in one unit. Ignore rows are counted
only.

Call `write_port_work_unit` once per unit with:

- `work_unit_id`: `{{ event.correlation }}:unit-<nn>`
- `surface_ids`: space-separated member `surface_id`s
- `verdict`: `implement` or `shaped-stub`
- `status`: `ready`
- `attempt`: `1`
- `branch`: `gents/{{ event.correlation }}/unit-<nn>` (unique and Git-ref
  safe; use the same two-digit ordinal as `work_unit_id`; never copy the `:`
  separators from `work_unit_id` into a branch; do not reuse the job PR branch)
- `expected_total`: number of work units, same on every unit
- copy `area`, `grok_call_sites`, `grok_wire`, `gents_docs`, `live_prompt`,
  `live_expect`, and `evidence` verbatim from the members
- copy `repository_id` and `base_sha` from the surfaces; do not invent `HEAD`
- `title` and `instructions` naming the Grok call sites to implement

Then call `write_port_plan` once with `work_unit_count`, `implement_count`,
`stub_count`, `ignore_count`, the same `expected_total`, and a short
`summary`. If there are no implement/stub rows, write the same `status=skipped`
non-executable sentinel described above and set the plan `expected_total=1`.
Never write a `ready` sentinel. Do
not supply `run_id`.
