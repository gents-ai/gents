---
name: package-pack
description: Load when a working app should become a pack, or when installing or changing a pack. Export, check, install, versions.
---

# Package a pack

A pack carries an app to another node, or back to this one, in one install. Make it from what already works.

## Export what you built

1. Clean up first: remove documents, pages, plugins and collections you tried and dropped (`config help cleanup`, `schema` help `collections`). What is live is what ships.
2. Run `pack export NAME` (`config help pack`). Select the app's documents by ID, tag or collection. The export follows references, so naming the triggers and the page usually brings the rest. Tagging the app's documents as you build makes this one option.
3. Read the receipt. It lists what was written and anything it could not carry. Agents become inference slots the installer binds.
4. `pack check`, then `pack build`. Fix every finding.
5. Prove it: preview the install and read what it would create. If you can, install it where the app does not already exist and run the app once.

## Use a pack

`pack search` and `pack get` show what a pack holds before you install it. `pack preview install` lists what it creates and what its plugins ask for. Install binds inference slots as `--inference-slot NAME=PROFILE_ID`. You can install any pack yourself.

Installing a plugin does not select it. Add it to the Tools of each agent that needs it, then use a fresh request: changes apply to later requests only.

## Change a pack

Change the live documents, test, and export again with a higher `version`. For a pack kept as a directory, edit the source, check, build and reinstall. Don't edit the installed copy.

## Directory layout

| Part | Location | Declared under |
| --- | --- | --- |
| Collections | `schemas/*.graphql` | `assets` |
| Agents, tools, surfaces, tasks, triggers | `pack_config.json` | `config` |
| Prompts | Markdown that `pack_config.json` names | `assets` |
| Page | `ux/<name>/plugin.js` and `plugin.css` | `ux` and `assets` |
| Compiled code | `plugins/` | `plugins` and `assets` |

Only files the manifest declares travel with the pack. A page's surfaces and tools are listed under `ux[].data`, and a plugin's collections are declared in the pack's schemas.
