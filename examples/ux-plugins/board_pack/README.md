# board_pack

The shape of hermes-agent's kanban plugin on Gents, on documents. One UX
plugin (`ux/board/plugin.js`): a Board row in the main nav, the Board page
it opens, and a `::board{id="…"}` card the agent can drop into chat.

Where the Hermes plugin calls `ctx.rest('/board')` into a FastAPI router
(`plugins/kanban/dashboard/plugin_api.py`), this one calls `ctx.data`: the
`board` surface that `../board_docs` installs on the node is its data half,
and the manifest's `ux[].data` names the surface and the four tools the
page may call. The page observes documents and writes documents; it runs no
processing of its own, and an agent whose `Tools.datastore` selects the
same surface reads and writes the same board through the same four tools.

The data contract is written up in `gents-ux-plugins/DATA_CONTRACT.md`;
how this page uses it in `gents-ux-plugins/BOARD_DATA_CONTRACT.md`.

## Try it

```sh
gents pack check   examples/ux-plugins/board_docs
gents pack install ./examples/ux-plugins/board_docs --home ~/.gents

gents pack check   examples/ux-plugins/board_pack
gents pack install ./examples/ux-plugins/board_pack --home ~/.gents
```

The documents pack installs *into the node* (schema and surface); the
assets pack installs as a home record the desktop lists. Then in the
desktop: Agent ▸ UX Plugins ▸ Reload, turn **Board** on, and the Board row
appears in the main nav. Give an agent the surface (`Tools.datastore`
selecting `board`), ask it "add a task to the board called X", and the
page shows it on the next refresh; ask "show me the board card for task
t1" and it emits `::board{id="t1"}`.

## Configuration

No `pack_config.json`: an `assets`-kind pack carries only its UX plugin.
The documents, the schema and the surface are `../board_docs`'s.

## Layout

```
manifest.json          kind: assets; ux[0].data names the surface and tools
ux/board/plugin.js     the plugin (plain ESM, SDK imports only, no build)
ux/board/plugin.css    its stylesheet, installed in the plugin's own frame
```
