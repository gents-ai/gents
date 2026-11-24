# engineer_datastore

## Configuration

Application datastore eval definition: the Engineer scopes its native query and write grants, then counts, pages, links, updates and deletes application records. The subject is the Engineer `gents init --engineer` makes at the commit under test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Usage

Run with `scripts/evals/run-ladder.sh engineer-datastore`. Held-out cases require `GENTS_EVAL_SPLITS=held_out`.
