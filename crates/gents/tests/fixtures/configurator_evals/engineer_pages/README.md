# engineer_pages

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). It holds
`config` (pages, datastore surfaces), and `charts` and `data_tables`, which
its Tools hide behind `script`.

A case that repairs or edits a page seeds its starting `DatastoreToolSurface`
and `Page` documents through `fixtures.documents`, which the trial runner
applies via desired state under the trial Node so the Engineer can read and
edit them natively under ACP alongside the fixture app collections and rows.

Page cases grade the stored Page and DatastoreToolSurface documents and, where
the user asks for it, a passing page lint. The trial image has no page
renderer, so what a page shows is graded from its module and data
declarations, not from a render. A chart case asks for a chart, so it
requires a completed `charts` call (made directly or inside `script`), and
grades the chart's numbers through the answer: a charts call inside `script`
leaves no output segment, and its stored result is the response JSON followed
by an image note, which no check can parse. Table cases grade the answer and
any file the user asked for.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| ticket-board-page | a page listing tickets with a maintenance count | `config` page create, a datastore surface, page lint | one Ticket Board page reading a Ticket query surface, showing title, lane and the maintenance count; lint admitted |
| rename-read-tool | rename a read tool and keep its page working | surface update, page data and module update, lint | the surface holds only the renamed tool; the page declares and queries it; lint admitted |
| restore-my-holdings | bring back a hidden page, keep a retired one hidden | page update, lint | My Holdings enabled and otherwise unchanged; the other two pages untouched; lint admitted |
| fix-stale-calibrations | a page that shows no rows | page data declaration repair, lint | the page declares the surface its module queries; module and the team page unchanged; lint admitted |
| clear-lint-finding | fix what lint flags without changing the page | page lint, module and CSS edit | the page still shows the same table and badge; colours use theme variables; lint admitted |
| dock-totals-bar | crates per dock as bars across two files | `charts` bar with aggregation | a completed charts call; each dock's combined total in the reply |
| tide-line | each day's high water as a line | `charts` line over a per-day maximum, a non-numeric reading | a completed charts call; each day's high in the reply |
| pie-slice-limit | whether 15 product lines fit one pie | `charts` help topics | the 12-slice limit and the "Other" slice in the reply |
| unbilled-parcels | shipped parcels with no live invoice | `data_tables` SQL over two CSVs (or any route) | the four parcels and their count; inputs unchanged |
| add-chart-keep-css | a bar chart added above a page's table | page module edit, lint | the page keeps its stylesheet, table and query, adds a BarChart of vessels; lint admitted |
| refund-aging | overdue refunds saved for finance | day/month dates, a join, `data_tables` output or a written CSV | the saved CSV holds exactly the five refunds; the count in the reply; inputs unchanged |
| harbor-arrivals-page | a page that lists moorings and depths and logs arrivals | page with `useQuery` and `useWrite`, surfaces over two collections, lint | the Harbour Arrivals page reads and writes, its surfaces cover Mooring and QuayDepth; lint admitted |
| rig-flow-scatter | flow against vane angle per operator | `charts` scatter with series and correlation | a completed charts call; 5.6 and ana in the reply |

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-pages`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
