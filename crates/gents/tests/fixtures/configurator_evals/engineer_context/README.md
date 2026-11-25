# engineer_context

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). One
`read_file` result holds at most the default 32,000 characters.

Cases cover `context_budget`, the stubs that replace an earlier request's tool
outputs (recovered with the `sessions` output read once the file they came
from is gone) and answers deep in long files. No case forces a compaction. Each
recovery case's first stage has the file read, so its rows are in the
session's history; the later stage grades only the answer.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| budget-headroom | how big the context is and when compaction starts, before a big paste | `context_budget` | the 75% threshold and the window size in the reply |
| cleared-handover-recovery | a detail from notes the user had cleared | `read_file`, `write_file`, `sessions` output of a stubbed result | the file cleared; Pier 3, then the Pier 4 gate code |
| manual-final-step | the last step of a long manual | `read_file` of a long file | the tag in the reply; the manual unchanged |
| cleared-manifest-recovery | who signed for a parcel on a cleared manifest | `read_file`, `write_file`, `sessions` recovery, a same-bay trap in the file still on disk | the counts and the cut file, then the outbound parcel and its signer |
| rotated-log-last-event | the last G3 badge from a rotated log | pulling events out of a long log, `sessions` recovery | the event count and the rotated file, then the last badge |
| slimmed-rotation-recovery | a field dropped when files were slimmed | `edit_file`/`write_file` on JSON, `sessions` recovery | each slimmed file's JSON, then the extension |
| route-fare-total | one route's takings for a day from a long ledger | a long ledger, exact route matching, refunds | the total and the entry count; the ledger unchanged |

Their files (15,694 and 8,537 characters) are under the default cap, so
`manual-final-step` and `route-fare-total` are single reads; no case reads
past the cap.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-context`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
