# engineer_automation_graphs

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). A case's
first stage asks the Engineer to build automation (or a manual Task); later
seed stages write the source records, and the settling stages grade the
TriggerFire, FireOutcome and AgentRequest rows those records cause, including
the request each record's template rendered and the session it landed in.

A seed stage waits for its own record to start a request, so under a correct
configuration every seeded record starts at least one: negatives are routed
to a second branch rather than left silent. Automation a case repairs or
inspects is built by its earlier stage, because the Engineer neither sees nor
owns evaluator-written Tasks; existing sessions are fixture documents.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-automation-graphs`;
held-out cases run only with `GENTS_EVAL_SPLITS=held_out`.
