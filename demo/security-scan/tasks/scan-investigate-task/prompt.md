Scan run {{ event.correlation }}, batch `{{ doc.batch_id }}` at
`{{ doc.scan_root }}`. Assigned files: `{{ doc.paths }}`.

Pre-scan hits for this batch:

{{ doc.hits }}

Instructions from the planner: {{ doc.instructions }}

Read every assigned file in full, then follow the data: callers,
consumers, and the places user- or network-controlled values enter the
flagged code. Use `lsp` for definitions, references, and hover when
semantic navigation beats text search. Use targeted read-only shell
commands (`git log`/`git blame`, `cargo test <specific test>`) when they
can settle a claim; background long commands. Do not run the full
workspace build or test suite; do not modify the tree.

Every successful tool result stays authoritative. Never repeat an
identical tool call or reread the same range; if exploration starts to
repeat, stop and write your findings.

For each genuine finding call `write_candidate_finding` with an exact
`path:line`, a verbatim code excerpt in `evidence`, severity from the
fixed vocabulary, `confidence` as an integer string 0–100 (only report
at 60 or above), and `finding_id` exactly
`{{ doc.batch_id }}:<finding-slug>`. At most six findings per batch —
prefer the highest-impact. Zero findings is a valid outcome.

Then call `write_investigation_result` exactly once as your final write:
`batch_id` exactly `{{ doc.batch_id }}`, `finding_count` as an integer
string matching your writes, and a two-sentence `summary`. Do not supply
`run_id` or `expected_total`; both are runtime-filled. Never retry a
successful write.
