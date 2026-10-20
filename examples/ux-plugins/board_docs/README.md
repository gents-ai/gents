# board_docs

The board's documents. A `documents` pack: two collections and one tool
surface, nothing that runs.

- `schemas/board.graphql` — `BoardTask` (`task_id`, `title`, `lane`,
  `created_at`) and `BoardMove` (`task_id`, `lane`, `moved_at`).
- `pack_config.json` — the `board` surface: `list_tasks` and `list_moves`
  (query), `add_task` and `move_task` (create).

The board is append-only. A task is created once, in a lane; every change
of lane is a `BoardMove`, and a task's current lane is its latest move (or
the lane it was created in). A move to `archived` takes it off the board.
Nothing is updated or deleted, which is what lets the surface stay a pair
of create tools and a pair of query tools — the same four tools a model is
given when an agent's `Tools.datastore` selects this surface.

Who reads and writes it:

- the `board` UX plugin in `../board_pack`, through `ctx.data`: its manifest
  names this surface and these four tools, and the desktop refuses anything
  else (see `gents-ux-plugins/DATA_CONTRACT.md`);
- any agent on the node whose `Tools.datastore` selects the `board` surface,
  through the same declarations.

## Install

```sh
gents pack check   examples/ux-plugins/board_docs
gents pack install ./examples/ux-plugins/board_docs --home ~/.gents
```

A documents pack installs *into a node*: the schema and the surface land on
the home's node (`--node-did <did>` targets another one; the desktop's
selected node must be the one that has them). Then install `../board_pack`
for the page.
