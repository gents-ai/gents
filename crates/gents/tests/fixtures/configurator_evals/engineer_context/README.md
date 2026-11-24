# engineer_context

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). One
`read_file` result holds at most the default 32,000 characters.

Cases cover `context_budget`, the stubs that replace an earlier request's tool
outputs (recovered with the `sessions` output read once the file they came
from is gone) and reading past the read cap. No case forces a compaction.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-context`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
