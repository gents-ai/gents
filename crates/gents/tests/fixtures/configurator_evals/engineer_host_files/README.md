# engineer_host_files

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, as `gents init --engineer` leaves the Engineer. Each case
writes its input files into the trial workspace, where the Engineer's host file
tools are rooted, and grades the files the stage leaves there with
`captured_files_match`: inputs it must not change by their sha256, outputs by
their text or JSON values. No check names a tool: reading inside a script, or
any other route, is graded only by the files and the reply.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| pickup-shortfall | which pickup orders are short, and by how much | read and join two JSON files | each short order with its missing units and the fully scanned count in the reply; inputs unchanged |
| dispatch-queue-merge | the current queue from two feeds in different time zones | JSONL reads, a written JSON file | the saved queue's records; inputs unchanged |
| change-request-apply | apply an approved change request to service configs | targeted `edit_file` across files, a skipped item | every config's exact text, no file created for the missing one, the change requests unchanged; the skipped item named |
| product-rename-prose | rename a product in prose but not in code | edits that skip code blocks and inline code | each doc's exact text; the changelog unchanged |
| station-renumber | renumber a station in live route files only | search across formats, exact-token edits, a directory left alone | each route file's text or JSON values; history unchanged |
| crew-lists-split | one file per crew of active members | CSV read, several written files | each list's lines, no file for a crew with no one active; roster unchanged |
| pump-error-tally | ERROR codes counted across nested logs | recursive listing, an excluded folder, a written report | the report's counts; logs unchanged |
| retry-callers-audit | which packages call a function under any alias | `grep` over a Rust workspace, re-exports | the package names written, one per line; sources unchanged |
| heron-current-decisions | the current agreed launch date, owner and budget | reading notes by their Date line, ignoring drafts | the saved decision's three fields; notes unchanged |
| berth-overlap-pairs | overlapping held bookings per berth | CSV reads, interval logic, a written list | the saved pairs; north and south named; inputs unchanged |
| beacon-live-minutes | live minutes per beacon in a shift | nested TSVs, merging and clipping intervals | the saved minutes per beacon; inputs unchanged |
| chiller-alarm-edit | change one value in a file with tabs and CRLF endings | a byte-preserving `edit_file` | the file's exact bytes; the example file unchanged |

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-host-files`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
