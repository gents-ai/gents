# configurator-l3-datastore

## Configuration

The Engineer sets up storage and uses it, or gives another agent a narrow
recordkeeping tool that the agent then uses.
The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).
No check names a tool the prompt did not ask for.

A stage in which the Engineer writes records it first has to grant itself
settles and may send one "OK, go ahead." turn before it is graded.

## Cases

| Case | User need | Graded by |
| --- | --- | --- |
| train-ds-schema-surface-select-call | an assistant that adds results and looks one up by key | the collection, the assistant's add and lookup tool with caller fields and no general query; the assistant saves and finds a record |
| val-ds-baseline | storage for two record types the Engineer writes to | both collections; a saved output read back |
| val-ds-create-only-surface | an assistant that can only add records | an add-only tool; the assistant saves a record |
| val-ds-two-collection-surface | an assistant that keeps two record types | add and lookup on both; a record of each saved, the input read back |
| val-ds-finish-partial-setup | a tool prepared first and handed to an assistant later | one prepared tool, reused when the assistant gets it; the assistant saves and finds a record |
| val-ds-automation-seed-to-output | an output written whenever an input arrives | a seeded input produces one output marked processed |
| val-ds-row-persists-across-stages | a record that is still there later | one row after the save and after the later lookup |
| held-ds-surface-on-new-agent | storage plus an add-only assistant | the collection, the add-only tool, a saved record the Engineer reads back |
| train-ds-parcel-scan-surface | depot scans the Engineer can add and look up | the collection; a scan saved and read back |
| train-ds-board-desk-reader | a read-only assistant that looks tickets up by desk | a lookup-only tool filtered by desk; the assistant lists the receiving desk's tickets, records unchanged |

## Usage

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents utils eval subject --out <subject>
gents utils eval run configurator-l3-datastore --cell engineer=<subject>:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
