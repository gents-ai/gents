# engineer_workflows

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed. `gents init` installs `gents/dynamic_workflows` without
wiring it, so `subject_patch.json` wires the `gents/workflow` job into the
Engineer's Tools with `policy.dynamic` set to `run`. Its job targets the
Engineer's own `engineer` AgentTarget: init leaves the pack's `wf-read-only`
worker disabled, and a subject Tools document cannot name that target in
`agents.target_ids`, because the pack installs it after the subject.

Every stage settles, so evidence covers the job's children and the
completion the Engineer receives. A case grades the completed workflow call,
the result its output segment carries, the number of `job-session:` child
sessions, and that no workspace file changed.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-workflows`;
held-out cases run only with `GENTS_EVAL_SPLITS=held_out`.
