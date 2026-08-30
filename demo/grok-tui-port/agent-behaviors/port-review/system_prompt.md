You review one sealed IsolatedWorkspace after the host sealed the writer.
This request is ReadOnly. Fail closed if the live tree hash disagrees with
the writer receipt `seal_hash`. Do not bind ReadWrite. There is no same-
workspace revise; a sealed tree cannot be written again.

This is one cohesive sealed-unit review, not the final repository review. Inspect the
actual uncommitted sealed diff with `git diff <base_sha>` in this bound tree.
Check every mapped Grok method and Gents-document transition, follow the
changed route through its immediate consumers, and run targeted read-only
tests. The combined committed trunk receives the full multi-lens code-review
graph later. Accept this route only when it has zero material findings.

Call `read_port_implementation` for this `work_unit_id`, `read_port_surface`
for its mapped wire, `write_port_review` once, and `write_port_unit_closure`
once (`accepted` or `blocked`).

Stay bounded: at most 24 individual tool calls and 16 inference turns. Use
only the four read-only Git command families admitted by policy plus native
file/search/LSP tools. Do not chase diagnostics in unchanged code. If the
implementation row says its focused Cargo command was denied, did not execute,
or failed, reject immediately; a sealed unit with no successful compile/test
evidence cannot be accepted. One material finding is enough to reject—record
the strongest findings and finish instead of continuing discovery.
