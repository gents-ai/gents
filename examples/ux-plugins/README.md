# UX plugin examples

Four packs and folders, each one shape the desktop accepts, ported from
hermes-agent's bundled desktop plugins (`apps/desktop/src/plugins/`):

| Example | Hermes original | Door | Form | What it shows |
|---|---|---|---|---|
| `hello_runtime/` | `hello-runtime/plugin.runtime.js` | dev (`<home>/ux-plugins/`) | plain ESM | the whole runtime pipeline in 25 lines: SDK import, one header action as data the shell draws, the click answered in the plugin's own frame |
| `focus_timer/` | `radio/plugin.js` | dev | plain ESM + css | the radio plugin's patterns: `ctx.storage` state (validated on read), a header action whose label is the clock, a page of its own, `ctx.setInterval`, a stylesheet beside the module; ships off like the original |
| `board_docs/` | (`plugin_api.py`'s store) | documents pack, into the node | schema + `pack_config.json` | the board's documents: `BoardTask` / `BoardMove` and the `board` surface with four tools — what both the page and an agent read and write |
| `board_pack/` | `kanban/plugin.tsx` | assets pack | plain ESM + css | the kanban plugin's shape: a nav row, the plugin's own page, a `::board` card, and `ctx.data` into the `board` surface (the Gents stand-in for `ctx.rest`); the manifest's `ux[].data` names what the page may call; ships off like the original |

A bundled fifth, `hello`, lives in the app at
`apps/gents-desktop/src/ui/ux-plugins/hello/plugin.js`: it ships off
(`defaultEnabled: false`) and, once enabled in Agent ▸ UX Plugins, shows
every first-wave area at once. Every plugin, bundled included, runs in a
sandboxed frame of its own.

## Running them

```sh
# dev door: copy a folder in, then Agent ▸ UX Plugins ▸ Reload
cp -r examples/ux-plugins/hello_runtime ~/.gents/ux-plugins/
cp -r examples/ux-plugins/focus_timer  ~/.gents/ux-plugins/

# pack door: the documents pack into the node, the assets pack into the home.
# `install` takes a pack spec, so a local path must be explicit (`./`, `../`,
# absolute, or a `.pack` file); a bare path reads as a registry coordinate.
# Without the CLI installed: `cargo run -q -p gents-cli --bin gents -- pack …`
gents pack check   examples/ux-plugins/board_docs
gents pack install ./examples/ux-plugins/board_docs --home ~/.gents
gents pack check   examples/ux-plugins/board_pack
gents pack install ./examples/ux-plugins/board_pack --home ~/.gents

# see what the desktop will receive
gents ux list
gents ux module focus_timer
gents ux module examples/board_pack/board
```

Every example passes `gents ux lint` and is covered by
`apps/gents-desktop/tests/ux-examples.test.ts`, which loads each one into an
in-process frame over the real port protocol and, for the board, drives its
`ctx.data` reads and writes.
