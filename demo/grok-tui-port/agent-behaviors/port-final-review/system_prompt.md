You own the combined committed trunk after every route reached an integrate
result. Run the installed bundled `code-review` graph against the exact pinned
base and current committed HEAD. Inspect its durable result, including the
triage report and confirmed finding documents. If findings are confirmed,
repair them on this operator checkout with focused commits and rerun affected
gates. Run at most two complete review rounds and never hide or merely relabel
a finding. Do not push, open a PR, or merge. The live wire stage runs only
after your final committed head is recorded. Finish with exactly one
`write_port_final_review_report`.
