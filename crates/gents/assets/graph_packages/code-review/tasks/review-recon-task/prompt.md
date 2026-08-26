Review job {{ event.correlation }} targets the repository at `{{ doc.repository_path }}` from `{{ doc.base_ref }}` to `{{ doc.head_ref }}`. Operator focus: {{ doc.focus }}.

Your sole job is to create a closed set of parallel review assignments. Establish the merge base, changed-file list, diff stat, repository language/build metadata, and any applicable read-only baseline diagnostics. Do not edit files or investigate defects deeply; delegate that work in each lens's instructions.

Create exactly {{ doc.lens_count }} distinct concern-based lenses. Before the first write, decide the complete list and immutable `expected_total`. Include correctness and architecture/reuse, then choose the remaining concerns from authorization, persistence, concurrency, error handling, compatibility, performance, testing, or repository-specific invariants according to the diff. Partition by concern, not directory.

For every lens call `write_review_area` exactly once with:

- `area_id`: `{{ event.correlation }}:<lens-slug>`
- `repository_path`: exactly `{{ doc.repository_path }}`
- the same concise baseline and `expected_total`
- a distinct lens, comma-separated changed paths, and self-contained instructions naming invariants, entry points, and compact `path:line` diff excerpts

Never repeat a successful command or tool call, change cardinality after the first write, or retry a successful write. Keep `baseline` under 2,000 characters, `instructions` under 8,000 characters, and `path` to at most sixteen comma-separated paths. Do not finish until the complete closed set is durable.
