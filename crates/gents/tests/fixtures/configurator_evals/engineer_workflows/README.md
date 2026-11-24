# engineer_workflows

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed. `gents init` installs `gents/dynamic_workflows` without
wiring it, so `subject_patch.json` wires the `gents/workflow` job into the
Engineer's Tools with `policy.dynamic` set to `run`. Its job targets the
Engineer's own `engineer` AgentTarget: init leaves the pack's `wf-read-only`
worker disabled, and a subject Tools document cannot name that target in
`agents.target_ids`, because the pack installs it after the subject.

Every case asks for one background job by name. Each stage settles, so the
evidence covers the job's helpers and the completion the Engineer receives. A
case grades the job with that name (`job_pin.handle`): it completed, and the
result its output segment carries holds the answer; the number of helper runs
that completed in `job-session:` sessions, as many as the user asked for; that
no workspace file changed when the user said to only read; and, where the user
asked to be told, the Engineer's reply after the job finished. A draft the
Engineer stopped or a job under another name is not graded.

## Cases

| Case | Split | User need | Features exercised | Graded by |
| --- | --- | --- | --- | --- |
| gate-count-recall | train | count entries with one helper, then ask what the job returned | spawn one child, read its text, result; reading a finished job | the job's gate_b is 4 with one helper and no file change; the later reply says 4 and one helper, with no new job or agent started |
| mail-count | train | add up a file with one helper and hear the total when it is done | spawn, read, result, the background completion turn | the job's unread is 29 with one helper and no file change; the reply after completion says 29 |
| pier-sums | train | two helpers total one file each, combined | parallel spawns, reading two children, fan-in | north 39 and south 51 with two helpers; the reply says both |
| late-desk-escalation | train | a second helper only if the first one's answer calls for it | sequential steps that branch on a child's text | late_ids [N1, N6] and escalated false with one helper only |
| calibration-review | validation | combine two helpers' reports under a rule | parallel spawns and a fan-in with logic | spreads {LST-47: 13} and stale [LCH-82] with two helpers |
| freeze-check | validation | one helper checks readings and finds none below zero | an empty result that still comes from a helper | frozen [] from a job whose one helper made at least one tool call; no file change |
| west-loading-reconcile | held_out | reconcile two helpers' lists | parallel spawns, set difference in the step | missing [M09, M12] and unexpected [M14] with two helpers |
| reefer-tally-fan-in | held_out | one helper per file, then the total and the busiest | fan-out sized by the files, fan-in | reefer_total 47 and busiest west with three helpers; the reply says both |

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-workflows`;
held-out cases run only with `GENTS_EVAL_SPLITS=held_out`.
