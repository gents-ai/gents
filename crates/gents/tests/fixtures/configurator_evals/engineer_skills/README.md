# engineer_skills

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, so the pack_builder skills are in the Engineer's catalog;
`subject_patch.json` adds four domain Skill documents to that catalog. A skill
counts as loaded when its rendered instructions appear in the session's tool
output.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-skills`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
