# board_store

The task board. One call, one action:

- `{"action":"list","root":"<folder>"}` → `{"tasks":[{"id","title","lane"}]}`
- `{"action":"add","root":"<folder>","title":"...","lane":"triage"}` → the new task
- `{"action":"move","root":"<folder>","id":"3","lane":"running"}` → the moved task
- `{"action":"remove","root":"<folder>","id":"3"}` → `{"removed":"3"}`

Lanes are `triage`, `ready`, `running`, `done`. The board is `board.json`
in the bound folder. After you move or add a task, the person can see it
on the Board page; to point them at one task in chat, write a line that is
exactly `::board{id="3"}`.
