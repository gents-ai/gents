# UX plugin examples

Each folder is one shape of UX plugin the desktop accepts. Every plugin,
bundled or installed, runs in a sandboxed frame of its own.

| Example | Installed through | Form | Shows |
| --- | --- | --- | --- |
| `hello_runtime/` | dev folder (`<home>/ux-plugins/`) | plain ESM | the runtime pipeline: SDK import, one header action as data the shell draws, the click answered in the plugin's own frame |
| `focus_timer/` | dev folder | plain ESM + css | `ctx.storage` state validated on read, a header action whose label is the clock, a page of its own, `ctx.setInterval`, a stylesheet beside the module; ships off |
| `board_docs/` | documents pack, into the node | schema + `pack_config.json` | `BoardTask` / `BoardMove` and the `board` surface with four tools, which both the page and an agent read and write |
| `board_pack/` | assets pack | plain ESM + css | a nav row, the plugin's own page, a `::board` card and `ctx.data` into the `board` surface; the manifest's `ux[].data` names what the page may call; ships off |
| `board_dashboard/` | assets pack | plain ESM | a dashboard built only from SDK blocks over the same `board` surface |

The bundled `hello` plugin lives in the app at
`apps/gents-desktop/src/ui/ux-plugins/hello/plugin.js` and ships off
(`defaultEnabled: false`).

The `README.md` in each pack folder is a pack asset its manifest lists; it is
what a person installing the pack reads.

## Checking a change

Every example passes `gents config pack ux lint`, and the packs pass
`gents config pack check <dir>`. `apps/gents-desktop/tests/ux-examples.test.ts`
loads each example into an in-process frame over the real port protocol and,
for the board, drives its `ctx.data` reads and writes:

```sh
npm --prefix apps/gents-desktop test -- tests/ux-examples.test.ts
```

To try them by hand, copy a dev-folder example into `<home>/ux-plugins/` and
reload in Agent ▸ UX Plugins, or install a pack with
`gents config pack install ./examples/ux-plugins/<pack> --home <home>`
(`install` reads a bare path as a registry coordinate, so a local path starts
with `./`, `../` or `/`). `gents config pack ux list` and
`gents config pack ux module <name>` show what the desktop receives.
