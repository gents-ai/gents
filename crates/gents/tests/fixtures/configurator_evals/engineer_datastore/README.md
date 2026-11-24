# engineer_datastore

## Configuration

Application datastore eval definition: the Engineer scopes its native query and write grants, then counts, pages, links, updates and deletes application records. The subject is `../ladder/engineer_subject`.

## Usage

Run with `scripts/evals/run-ladder.sh engineer-datastore`. Held-out cases require `GENTS_EVAL_SPLITS=held_out`.
