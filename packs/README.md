# Gents packs

A pack distributes behaviors, tasks, datastore tools, schemas, prompts and
supporting assets. It may contain a compiled graph, a document-driven worked
scenario, or reusable assets alone. All bundled packs have one source here.

```sh
gents pack list
gents pack show code_review
gents pack install code_review --home <initialized-home>
gents graph run code_review --repo . --base origin/main --head HEAD
gents pack install mailbox --home <initialized-home>
gents pack run pipeline --http-port 19191 --keep-home
```

## Installation and execution

`pack install` resolves bundled assets by name, without a source checkout.
Graph packs use the runtime graph installer; document packs use schema-first
desired-state application. Neither submits scenario seed documents nor prunes
unrelated configuration. Enabled schedules and triggers can execute when their
configuration is applied to a serving node; installation is not a dry run.
Asset-only packs are materialized beneath `<home>/packs/`.
Declared graph dependencies are installed before the document pack; this is
not an atomic multi-package transaction. Failures remain visible and installs
can be retried through the existing owners.

Graph role bindings inherit the target principal's default behavior; use
`--bindings` for explicit graph bindings. Document packs use their authored
configuration and `${VAR}` / `${VAR:-default}` substitutions. They bind to the
target node through existing identity-rebinding checks; concrete DIDs require
explicit `--force-rebind-concrete-did`. Review tool declarations and host
authority before installing untrusted content. External dependency commands
are documentation, never automatically executed.

`pack run`, `init`, and `seed` operate `experiment.json` scenarios. A source
directory can be used while authoring; bundled names are materialized into the
local pack cache when no source directory is selected. Run artifacts are under
the resolved pack's `runs/<job_id>/`. Repository-specific scenarios still need
their documented checkout, tools and bindings; bundling does not provision a
compiler or an external model endpoint.

`gents graph run/watch/result/cancel/enable/disable` remain graph operations.
The former `graph install/catalog` and `demo` pack subcommands are removed.
`gents demo` remains the interactive fleet demonstration only.

## Authoring standard

Each `packs/<snake_case_name>/manifest.json` declares:

- `manifest_version`, `name`, semantic `version`, and `description`;
- `authors`, `tags`, and `kind` (`graph`, `documents`, or `assets`);
- explicit `assets` and package-name `dependencies`;
- for graph packs, compiler version, roles, schemas, intent and capabilities.

Register the name in `packs/catalog.json`; adding a pack needs no Rust changes.
The build watches that index and declared assets, not the whole directory tree,
so accumulating run logs does not continuously rebuild the runtime.

Use snake_case directories and filenames, except conventional ecosystem names
such as `README.md` and `Cargo.toml`. No old-name aliases are provided. Changing
filesystem handles does not require renaming Task IDs or database collections.
Desired-state roots and export use snake_case collection directories too.

A README must explain purpose, installation, bindings/prerequisites, tool and
workspace authority, inputs/outputs, completion/failure semantics, validation,
and operational history. Graphs must include a Mermaid diagram. Refresh and
check generated topology sections with:

```sh
node scripts/check_packs.mjs --write-diagrams
node scripts/check_packs.mjs
```

For compiled graphs the diagram reflects capability edges. For document-driven
scenarios it reflects declared trigger edges; document writes and callbacks
must additionally be explained in prose. A trigger diagram is not proof of
runtime completion behavior.

Keep concise run summaries, reviewed outputs and issue links. Never bundle
`runs/`, node homes, credentials, build caches or raw logs. Package embedding
uses declared assets, not recursive discovery of an operator's workspace.
Source resolution is separate from installation; GitHub and registry sources
are future work, not implemented download features.

## Worked examples

| Pack | Purpose |
| --- | --- |
| [background_continuation](background_continuation/README.md) | Child completion and parent wake |
| [code_review](code_review/README.md) | Reusable reviewed-evidence graph |
| [defending_code](defending_code/README.md) | Discovery, verification and patch review |
| [graph_pipeline](graph_pipeline/README.md) | Compiler evaluation fixtures |
| [grok_tui_port](grok_tui_port/README.md) | Large implementation case study and probes |
| [lsp_rust](lsp_rust/README.md) | Rust language-server integration |
| [mailbox](mailbox/README.md) | Explicit human-attention tool surface |
| [pipeline](pipeline/README.md) | Minimal document-trigger pipeline |
| [repo_maintenance](repo_maintenance/README.md) | Repository work through reviewed PR |
| [security_scan](security_scan/README.md) | Whole-codebase discovery and verification |
| [web_deep_research](web_deep_research/README.md) | Reusable research graph |
