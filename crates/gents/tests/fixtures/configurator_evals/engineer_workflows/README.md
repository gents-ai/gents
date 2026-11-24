# engineer_workflows

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Subject fixtures

- `gents/dynamic_workflows` is turned on (`subject.enabled_core_packs`) as
  `gents config pack starter --enable gents/dynamic_workflows` turns it on:
  init installs it but leaves it off, and a user who wants workflows runs
  that command. It wires the `gents/workflow` job into the Engineer's Tools
  with its pack's job policy and turns on the pack's `wf-read-only` worker,
  the job's target.

Every stage settles, so evidence covers the job's children and the
completion the Engineer receives. A case grades the completed workflow call,
the result its output segment carries, the number of `job-session:` child
sessions, and that no workspace file changed.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-workflows`;
held-out cases run only with `GENTS_EVAL_SPLITS=held_out`.
