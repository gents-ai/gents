# engineer_skills

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). The
pack_builder skills are in its catalog. A skill counts as loaded when its
rendered instructions appear in the session's tool output.

## Subject fixtures

- `four-task-skills`: four Skill documents a user wrote for their own work
  (courier routing, archive and specimen-log retention, ferry claims),
  attached to the Engineer's Context beside the core packs' skills. A user's
  node holds the skills its owner adds; the cases test that the Engineer
  finds and follows the one a task needs.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-skills`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
