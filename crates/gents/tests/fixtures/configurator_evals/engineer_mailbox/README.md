# engineer_mailbox

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). Mailbox
items are read as the operator (the evaluator), their only reader; the
Engineer cannot read them back, so a repeated turn is graded on the items
the operator sees. The surface files one notice per request, and a question
is its own item.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-mailbox`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
