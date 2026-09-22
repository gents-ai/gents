# DefraDB Explorer (embedded build)

Built `dist/` of [gents-ai/defradb-explorer](https://github.com/gents-ai/defradb-explorer)
(branch `gents-embed`, a fork of
[sourcenetwork/defradb-explorer](https://github.com/sourcenetwork/defradb-explorer)),
compiled with `VITE_EMBEDDED=1`: the explorer pins its connection to the origin
that serves it and hides connection management.

`gents serve` embeds these files and exposes them at `/explorer/` on the
runtime HTTP listener, same-origin with the DefraDB API — so no CORS
configuration is needed. The desktop app's "DB Explorer" developer option
opens this page.

- Pin: see `PIN` (defradb-explorer commit SHA)
- Refresh: `scripts/refresh-defradb-explorer.sh` from the gents repo root
  (set `DEFRADB_EXPLORER` to your checkout if it is not at `../defradb-explorer`)
