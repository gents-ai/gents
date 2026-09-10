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
gents pack prune mailbox
gents pack run pipeline --http-port 19191 --keep-home
```

## What a pack is made of

A pack directory holds `manifest.json` and the assets that manifest declares.
Nothing that is not declared travels, so the manifest is the whole description
of the pack, and the pack's digest is computed over exactly what it declares.

```json
{
  "manifest_version": 1,
  "name": "shipping_tools",
  "version": "0.1.0",
  "description": "What this pack is for",
  "authors": ["you"],
  "tags": ["tools"],
  "kind": "tools",
  "assets": ["README.md", "tools/format_check.wasm"],
  "tools": [
    {
      "name": "format_check",
      "description": "What the model is told this does",
      "module": "tools/format_check.wasm",
      "source": "tools/format_check",
      "input_schema": { "type": "object", "properties": {} },
      "manifold": { "fs": { "ReadOnly": ["/workspace"] }, "net": "None" }
    }
  ]
}
```

`kind` is `documents`, `graph`, `assets`, or `tools`. A `tools` pack installs
no documents of its own: it exists to ship capabilities, and its tools are
callable from any pack in the same home, so one pack can build on another's
capabilities instead of vendoring a copy.

## Tools

A tool is compiled WASM that runs sandboxed on Afterburner. The same admitted
tool is callable two ways, from one definition: as a deterministic stage in a
graph, and as an ordinary tool a model can pick. A tool declares:

- `module`, the compiled artifact inside the pack. It must also appear in
  `assets`, so the pack's own digest covers it and nothing can be swapped
  underneath the name it was admitted under.
- `source`, optionally, where the module is built from. `gents pack build`
  compiles it.
- `input_schema`, which is what a model is shown.
- `manifold`, what the tool asks the sandbox to allow. Absent means it asks
  for nothing, which is right for a pure transform. An operator's ceiling
  narrows this at admission and can never widen it. A tool may not listen on
  a port: a pack's tools are called, never served.

The call ABI is deliberately narrow: canonical JSON arguments arrive on
standard input, one JSON value is written to standard output, and standard
error is diagnostics.

## Building and publishing

```sh
gents pack build packs/shipping_tools        # compile the tools, write one .afb
gents pack publish shipping_tools-0.1.0.afb  # push it to the registry
gents pack install shipping_tools            # from the registry, anywhere
```

A built pack is a single `.afb`: the same artifact every Afterburner tool is
already published and served as. That is deliberate. It means a registry needs
no second format to carry packs, and a pack that ships compiled tools is one
artifact rather than an archive plus a pile of modules.

The default registry is `https://packs.gents.xyz`, overridable per command and
by `GENTS_REGISTRY`.

A pack installed from a registry is the same pack as the one compiled into a
binary: the digest is over the declared contents, never over the container, so
neither the route a pack took nor the compression it arrived under changes
what it is. A download is checked against the digest the registry advertised
before it is opened.

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
Only document packs currently declare package dependencies, and those must be
graph packs. Graph/asset dependency lists are rejected; recursive installation
is not silently implied.

Graph role bindings inherit the target principal's default behavior; use
`--bindings` for explicit graph bindings. Document packs use their authored
configuration and `${VAR}` / `${VAR:-default}` substitutions. They bind to the
target node through existing identity-rebinding checks; concrete DIDs require
explicit `--force-rebind-concrete-did`. Review tool declarations and host
authority before installing untrusted content. External dependency commands
are documentation, never automatically executed.

`pack run`, `init`, and `seed` operate `experiment.json` scenarios. A lexically
normalized source directory with a snake_case leaf name can be used while
authoring; bundled names are materialized into the local pack cache when no
source directory is selected. Run artifacts are under the resolved pack's
`runs/<job_id>/`. Repository-specific scenarios still need their documented
checkout, tools and bindings; bundling does not provision a compiler or an
external model endpoint.

`manifest.json` is the sole package dependency declaration, including for
scenario runs. `experiment.json` may configure scenario-specific graph model
bindings, but cannot declare another dependency list.

The bundled scenario asset cache is separate from the runtime home selected
with `pack run --home`: it lives under the default Gents home's `packs/` tree.
The distribution digest covers all declared assets, including documentation;
the graph execution digest covers only the graph's referenced inputs. Both use
the existing graph asset hashing routine. Filesystem cache names use the hex
portion of the shared `sha256:` digest format. `pack prune <name> [--home <home>]`
removes superseded generated versions with no `runs/`. It takes the exclusive
per-pack cache lock; scenario operations retain a shared lock for their full
lifetime. Versions holding run history and directories without Gents'
ownership marker are retained.
Omit `--home` to prune the same default cache used by named scenario runs; pass
the same explicit `--home` used to install an asset pack when pruning that
cache.

`gents graph run/watch/result/cancel/enable/disable` remain graph operations.
The former `graph install/catalog` and `demo` pack subcommands are removed.
`gents demo` is removed; there is no alternate demo shell or compatibility command.

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
Use loopback defaults or required environment inputs for local inference;
do not ship enabled backends defaulting to private lab addresses.

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
