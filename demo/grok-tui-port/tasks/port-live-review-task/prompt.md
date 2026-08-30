Run {{ event.correlation }} closed its live probe ledger.

{{ group.docs }}

Call `read_port_final_review_report`, `read_port_surface`, and
`read_port_live_result` for the complete ledgers. Build the exact set of
surface IDs whose verdict is `implement` or `shaped-stub`. Reject duplicate,
missing, or extra probe surface IDs; a green review with zero non-ignore
surfaces must have the single
blocked `surface_id=none` sentinel. Count a `passed`
row as failed when `grok_wire_observed` or `gents_docs_observed` is empty or
does not match that surface's `live_expect`.

Call `write_port_live_report` once with counts, `expected_count` from the
final review, actual `observed_count`, `coverage_complete=true` only for exact
unique coverage, and `final_review_head` from the report. Any coverage defect
must increment `failed_count`. Do not supply `run_id`.
