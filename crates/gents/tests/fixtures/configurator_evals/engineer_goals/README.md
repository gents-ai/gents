# engineer_goals

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed; `subject_patch.json` turns goal creation on. The runner
cannot create a goal on a trial's session, so each case starts without one and
the Engineer creates any goal itself. Goal stages settle, so the evidence
covers the goal's continuation requests.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-goals`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
