Run {{ event.correlation }} has a closed ledger: {{ doc.candidate_count }} candidates, {{ doc.confirmed_count }} confirmed, and {{ doc.refuted_count }} refuted.

Verifier summary: {{ doc.summary }}

Load confirmed findings with `read_finding` and persist one `write_triage_report`. Use the ledger's confirmed/refuted counts; `high_priority_count` counts confirmed findings whose severity is Critical or Major. Report zero findings when appropriate and disclose any blocking review limitation. Run identity is runtime-filled.

Resume missing work from the existing Goal and history. Once the report is persisted, call `update_goal` with `status="complete"`.
