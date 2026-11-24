# engineer_script

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, as `gents init --engineer` leaves the Engineer: the
`gents/script` plugin is on its Tools with caller-tool access, and `charts`,
`data_tables` and `browser_exec` are offered only inside a script as
`tools.NAME`. No prompt mentions `script`: each case is a job where one script
is the cheap route (many files or records, a loop of edits, a hidden tool),
and every check grades what the user gets, never which tools were called.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| wet-lots-audit | wet lots not yet released before a handoff | `query` over two collections, join on the newest revision, paging past one page | the four lot ids and their count in the reply |
| sensor-recovery | total of the valid readings across a manifest of files | batched `read_file`, a failing read caught, skipping junk lines | the total and both unreadable files named; inputs unchanged |
| ticket-bulk-close | close every stale support ticket | `read_file` over 30 files, `edit_file`/`write_file` in a loop | each stale ticket closed with its other fields kept, every other ticket byte for byte; the count |
| cold-bin-notices | an inbox note listing the warm cold bins | `query`, `file_mailbox_item` | one open item with the title, listing exactly the four warm bins in the asked format |
| dock-totals-file | a per-dock crate report saved as JSON | CSV read and sum, `write_file` | the saved JSON's values; the busiest dock in the reply; input unchanged |
| freight-pagination | weight per route over 64 records | `query` paging | all three route totals and the heaviest route |
| chrome-tabs-setup | what to switch on before working in the user's Chrome | `browser_exec` help through `script` | `chrome://inspect` and the Allow dialog in the reply |
| retired-bins | retired bins with no active assignment | `read_file` joined with `query` | the three bins and their count; inventory unchanged |
| manifest-overlap | crates on both manifests and not on hold | three file reads, set logic, a messy hold list | the three crates and their count; files unchanged |
| relay-precedence | which relays end up on | two collections, override precedence, null as off | the six relays and their count |
| lab-lock-audit | lab locks open now or never seen | `list_files`/`glob` over nested logs, latest sequence wins | the three locks and their count; files unchanged |

A web search, fetch or crawl cannot run in a `--network none` trial, so no
case uses them; `browser_exec` is reached only through its offline help.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-script`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
