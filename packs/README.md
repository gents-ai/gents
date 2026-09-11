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
  "name": "shipping_plugins",
  "namespace": "acme",
  "version": "0.1.0",
  "description": "What this pack is for",
  "authors": ["you"],
  "tags": ["plugins"],
  "kind": "plugins",
  "assets": ["README.md", "plugins/format_check.afb"],
  "plugins": [
    {
      "name": "format_check",
      "description": "What the model is told this does",
      "artifact": "plugins/format_check.afb",
      "source": "plugins/format_check",
      "language": "rust",
      "input_schema": { "type": "object", "properties": {} },
      "manifold": { "fs": { "ReadOnly": ["/workspace"] }, "net": "None" }
    }
  ]
}
```

`namespace` is the registry namespace this pack publishes under, and the
other half of its coordinate (`acme/shipping_plugins`). It is optional and
`gents` when absent, so a first-party pack does not repeat it. A pack's
plugins install under the pack's namespace, so two packs from different
namespaces may each carry a `format_check` without one replacing the other.

`kind` is `documents`, `graph`, `assets`, or `plugins`. A `plugins` pack
installs no documents of its own: it exists to ship capabilities. Its plugins
land in the same store `gents plugin install` uses, one store per home, so
they are callable by name whichever pack put them there and one pack can
build on another's capabilities instead of vendoring a copy.

## Plugins

A plugin is a complete Afterburner `.afb`: publishable and installable on its
own, and also carried inside a pack as one of its declared assets. It is the
one artifact every language Afterburner compiles down to, since some of them
(Python to an emscripten-pyodide bundle, for instance) have no bare-`.wasm`
form to ship instead, and only Afterburner's own runtime knows how to
dispatch every one of those shapes.

Today a plugin is called directly, by name (`gents plugin run`). Offering the
same admitted plugin to a graph stage and to a model as an ordinary tool is
the reason it is one definition rather than two, and neither of those call
paths is wired yet. A plugin declares:

- `artifact`, the compiled `.afb` inside the pack, under `plugins/`. It must
  also appear in `assets`, so the pack's own digest covers it and nothing can
  be swapped underneath the name it was admitted under.
- `source`, optionally, where the artifact is built from. `gents pack build`
  compiles it through Afterburner, whatever language `language` names.
- `language`, the source language `source` is written in: `rust`, `go`,
  `c`, `cpp`, `python`, `ruby`, `js`, or `ts` (see
  `afterburner::cli::compile::lang::SourceLang` for the exact accepted
  spellings). Required even for a plugin that ships only a compiled artifact.
  Every one of them compiles, and every one of them runs under the bounds a
  call applies. What decides that is the compiled artifact, not the language
  name: whether a given `.afb` can be run with `stdin`, fuel, memory, a wall
  clock and its manifold grants all enforced is asked of Afterburner itself
  at admission, and a plugin that cannot be is refused by name rather than
  run with a bound silently missing.
- `input_schema`, which is what a model is shown.
- `manifold`, what the plugin asks the sandbox to allow. Absent means it asks
  for nothing, which is right for a pure transform. An operator's ceiling
  narrows this at admission and can never widen it. A plugin may not listen
  on a port: a pack's plugins are called, never served.

The call ABI is deliberately narrow: canonical JSON arguments arrive on
standard input, one JSON value is written to standard output, and standard
error is diagnostics.

## Building and publishing

```sh
gents pack build packs/shipping_plugins           # compile the plugins, write one .tar.gz
gents pack publish shipping_plugins-0.1.0.tar.gz  # push it to the registry
gents pack install acme/shipping_plugins          # from the registry, anywhere
```

A plugin is also managed on its own, without a pack around it:

```sh
gents plugin build ./format_check          # compile one .afb
gents plugin publish format_check-0.1.0.afb
gents plugin install acme/format_check
gents plugin list
gents plugin run acme/format_check --input '{"path":"src"}'
gents plugin remove acme/format_check
```

Installing a pack installs its plugins into the same store `gents plugin
install` uses, so a plugin that arrived inside a pack is runnable by name
exactly like one installed alone.

A built pack is a single gzip-compressed tar: a plain container any archive
tool can read, holding `manifest.json` and every asset the manifest declares,
including each plugin's compiled `.afb`. The registry at
`https://packs.gents.xyz` serves both packs and plugins, so a plugin
published on its own uses the same `.afb` a pack carries internally, and a
pack that ships plugins is one artifact rather than an archive plus a pile of
modules.

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
explicit `--force-rebind-concrete-did`. Review plugin declarations and host
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
