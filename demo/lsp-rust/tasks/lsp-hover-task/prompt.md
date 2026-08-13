Job {{ doc.job_id }} fired by trigger {{ event.trigger_id }} on
{{ event.source_collection }}.

{{ doc.prompt }}

This workspace is the Gents repository. You have an `lsp` tool backed by
rust-analyzer. Hover looks up `symbol` on a **1-indexed `line`** — if you
omit `line` it searches from the top of the file, but you should resolve
the line first. Do not guess types or comments.

Required sequence:

1. `action=symbols` on `crates/gents/src/toolset/shared/command.rs`.
   Read the 1-indexed line for `meet`.
2. `action=hover` on that file with `symbol=meet` and that line.
   Quote the documented rank order.
3. `action=symbols` on `crates/gents/src/toolset/lsp/auth.rs`.
   Read the 1-indexed line for `lsp_advertised`.
4. `action=hover` on that file with `symbol=lsp_advertised` and that line.
   Quote the signature.
5. After both hover results have returned, start a **new tool turn** and call
   `action=status` by itself. Do not batch status with symbols or hover. Status
   must say `rust-analyzer (ready)`; `configured, not started` does not count.

Quote the hover text. Reply DONE when those calls have completed.
