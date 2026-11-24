# engineer_explain

Query plans: scan and index evidence over application records, without and
with execution. The subject is the Engineer `gents init --engineer` makes at
the commit under test, every core pack installed as init installs them
(`scripts/evals/suites.py`).

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| indexed-versus-scan | why a report is slow, and whether an index helps | `query` explain on an unindexed and an indexed collection | a plan with a scan node, then one naming an index; records unchanged |
| empty-report-execution | the plan of an empty report, then its run | `query` explain, then explain with execution | a plan without execution, then an executed plan; the reply says nothing matched |
| recover-and-refuse | a plan asked for with wrong field names, then its run | schema discovery, `query` explain, `query` | a plan on the real fields; the one closed delivery in the reply |

`recover-and-refuse` no longer has a stage that asks for plans over credential
collections: a reply that leaks nothing cannot be told from one that does
without a negative final-message check, which the suite lacks.
