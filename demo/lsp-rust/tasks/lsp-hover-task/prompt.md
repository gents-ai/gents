Job {{ doc.job_id }} fired by trigger {{ event.trigger_id }} on
{{ event.source_collection }}.

{{ doc.prompt }}

This workspace is the Gents repository. Answer from rust-analyzer via the
`lsp` tool only. Do not guess types or comments.

1. Hover `CommandNetworkMode::meet` in `crates/gents/src/toolset/shared/command.rs`
   (`action=hover`, symbol `meet`). Quote the documented rank order.
2. Hover `lsp_advertised` in `crates/gents/src/toolset/lsp/auth.rs`
   (`action=hover`, symbol `lsp_advertised`). Quote the signature.
3. Call `lsp` `action=status`.

Quote the hover text. Reply DONE when those calls have completed.
