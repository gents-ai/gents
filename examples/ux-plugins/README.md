# UX plugin examples

Three plugins, each one shape the desktop loader accepts, ported from
hermes-agent's bundled desktop plugins (`apps/desktop/src/plugins/`):

| Example | Hermes original | Door | Form | What it shows |
|---|---|---|---|---|
| `hello_runtime/` | `hello-runtime/plugin.runtime.js` | dev (`<home>/ux-plugins/`) | plain ESM | the whole runtime pipeline in 30 lines: SDK import, `jsx()` calls, one header chip |
| `focus_timer/` | `radio/plugin.js` | dev | plain ESM + css | the radio plugin's patterns: `ctx.storage` state, a header chip with a popover, `ctx.setInterval`, a stylesheet beside the module |
| `board_pack/` | `kanban/plugin.tsx` + `plugins/kanban/dashboard/plugin_api.py` | pack | ESM + `.afb` | the kanban plugin's shape: a nav row, an agent-section page, a `::board` directive, and `ctx.plugin()` into the pack's own `.afb` (the Gents stand-in for `ctx.rest`) |

A bundled fourth, `hello`, lives in the app at
`apps/gents-desktop/src/ui/ux-plugins/hello/plugin.tsx` and proves every
first-wave area on each boot.

## Running them

```sh
# dev door: copy a folder in, then Agent ▸ UX Plugins ▸ Reload
cp -r examples/ux-plugins/hello_runtime ~/.gents/ux-plugins/
cp -r examples/ux-plugins/focus_timer  ~/.gents/ux-plugins/

# pack door: build (compiles the .afb), check (lints the ux module), install
gents pack build  examples/ux-plugins/board_pack
gents pack check  examples/ux-plugins/board_pack
gents pack install examples/ux-plugins/board_pack --home ~/.gents

# see what the desktop will receive
gents ux list
gents ux module focus_timer
gents ux module examples/board_pack/board
```

Every example passes `gents ux lint` and is covered by
`apps/gents-desktop/tests/ux-examples.test.ts`.
