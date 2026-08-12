Job {{ doc.job_id }} fired by trigger {{ event.trigger_id }} on
{{ event.source_collection }}.

{{ doc.prompt }}

Use the `lsp` tool. Required sequence:

1. `action=hover` on `src/lib.rs`, line 4, symbol `add`
2. `action=definition` on the same file and symbol
3. `action=status`

Quote the hover signature. Reply DONE when those three calls have completed.
