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
