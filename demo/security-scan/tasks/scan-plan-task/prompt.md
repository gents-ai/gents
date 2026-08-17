Scan run {{ event.correlation }} covers the repository at `{{ doc.scan_root }}`.
Focus: {{ doc.focus }}

Pre-scan inventory ({{ doc.candidate_files }} candidate files,
{{ doc.candidate_total }} candidates, slug counts {{ doc.slug_counts }},
overflow {{ doc.overflow_count }}):

{{ doc.candidates }}

Turn this inventory into between {{ doc.batch_min }} and {{ doc.batch_max }}
investigation batches. Rules:

1. About five files per batch. Group related files: same slug family, same
   module or subsystem, or the same vulnerability class. Precise-tier
   candidates lead: they get the earliest batch ids and the tightest
   grouping. Never mix a precise-tier file into a batch of purely noisy
   candidates when a tighter grouping exists.
2. Every candidate file appears in exactly one batch. If `overflow_count`
   is greater than zero, the inventory's path-only lines still get
   assigned — group them by directory affinity and say in the batch
   instructions that they carry no excerpt evidence.
3. You may use read-only file tools to check a path exists or gauge a
   file's size when deciding a grouping; do not read files to pre-judge
   findings, and never use a shell.
4. Decide the full batch list first, then write every batch. For each
   batch call `write_investigation_batch` with:
   - `batch_id`: `{{ event.correlation }}:batch-<two-digit-index>`
   - `scan_root`: exactly `{{ doc.scan_root }}`
   - `paths`: comma-separated relative paths (at most sixteen)
   - `hits`: the inventory lines for those files, verbatim
   - `instructions`: self-contained guidance (at most 8,000 characters)
     naming the fired slugs, what each slug means, and what to verify in
     these specific files
   - `expected_total`: the total batch count, identical on every write
5. Do not supply `run_id`; it is runtime-filled. Do not finish until every
   batch has been written successfully.
