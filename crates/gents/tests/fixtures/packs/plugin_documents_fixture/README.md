# plugin_documents_fixture

A `documents` pack that also ships one plugin: three inference slots
(`coordinator`, `worker`, `verifier`), one agent per slot, one task and one
schema. Test data, not a shipped pack.

## Plugin

`plugins/echo_fixture` returns an empty JSON object. The `.afb` is not
checked in: `gents pack build` compiles the source, and the gents crate's
tests write a constant-output module at the declared path in a temp copy.
