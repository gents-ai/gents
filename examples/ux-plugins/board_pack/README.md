# board_pack

A port of the shape of hermes-agent's kanban plugin to Gents: a UX plugin
(`ux/board/plugin.js`) that lands a Board page under the agent, a Board
nav row and a `::board{id="..."}` transcript card, and an `.afb` plugin
(`board_store`) it reads and writes the board through.

Where the Hermes plugin calls `ctx.rest('/board')` into a FastAPI router
(`plugins/kanban/dashboard/plugin_api.py`), this one calls
`ctx.plugin('board_store', { action, ... })`: the pack's own sandboxed
plugin is its backend half. The agent can call the same plugin as a tool
(`TOOL.md` teaches it), so a card the agent moves shows up on the page.

## Try it

```sh
gents pack build  examples/ux-plugins/board_pack
gents pack check  examples/ux-plugins/board_pack
gents pack install examples/ux-plugins/board_pack --home ~/.gents --grant-authority
```

Then in the desktop: Agent ▸ Board. Ask the agent "add a task to the board
called X" and it calls `board_store`; say "show me the board card for
task 1" and it emits `::board{id="1"}`.

## Configuration

No `pack_config.json`: a `plugins`-kind pack carries only its plugins and
UX plugins. The board lives in `board.json` under whatever folder the
operator allows (`gents plugin dirs add <folder> --access read_write`).
