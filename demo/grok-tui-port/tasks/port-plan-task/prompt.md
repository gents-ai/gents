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

For an accepted audit, this is a greenfield, tightly coupled protocol shim:
all non-ignore surfaces share the same new transport, session registry, and
test harness. Create exactly one executable work unit containing every
`implement` and `shaped-stub` row. Do not split by area, route, Grok call site,
or prospective Gents filename: parallel workspaces all start at the same base
and would create overlapping skeleton files that cannot be serially applied.
Ignore rows are counted but are not members.

Call `write_port_work_unit` exactly once with:

- `work_unit_id`: `{{ event.correlation }}:unit-01`
- `surface_ids`: every non-ignore `surface_id`, sorted and space-separated
- `area`: `grok_tui_shim`
- `verdict`: `implement` if any member is `implement`, otherwise `shaped-stub`
- `status`: `ready`
- `attempt`: `1`
- `branch`: `gents/{{ event.correlation }}/unit-<nn>` (unique and Git-ref
  safe; use `unit-01`; never copy the `:`
  separators from `work_unit_id` into a branch; do not reuse the job PR branch)
- `expected_total`: `1`
- `title`: the cohesive end-to-end Grok TUI shim
- `instructions`: require one compilable shim plus focused protocol and
  document-projection tests covering every member; plan shared file ownership
  before editing and do not open grok-build
- for each of `grok_call_sites`, `grok_wire`, `gents_docs`, `live_prompt`,
  `live_expect`, and `evidence`, concatenate every member in sorted
  `surface_id` order. Precede each complete value with
  `[surface_id=<member surface_id>]`; do not paraphrase or truncate the value
- copy `repository_id` and `base_sha` from the surfaces; do not invent `HEAD`

Then call `write_port_plan` once with `work_unit_count=1`,
`implement_count=1` and `stub_count=0` when any member is `implement` (or
`implement_count=0` and `stub_count=1` for an all-stub executable ledger),
the number of ignored surfaces as `ignore_count`, `expected_total=1`, and a
short summary that also records the member surface-verdict counts. If there
are no implement/stub rows, write the same `status=skipped` non-executable
sentinel described above and set the plan `expected_total=1`. Never write a
`ready` sentinel. Do not supply `run_id`.
