# configurator-l1-inference

## Configuration

The Engineer inspects its model and limits, changes them as asked, and the next
request runs under the new limits.
The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).
No check names a tool the prompt did not ask for.

## Cases

| Case | User need | Graded by |
| --- | --- | --- |
| train-basic-exec-profile-bind | a spending and time limit on the Engineer's own work | the limits on the execution its profile names, one backend, and a later request that completes under the new token limit |
| val-moderate-limits-reuse-backend | higher limits for long sessions on the same connection | as above |
| val-stream-settings | smoother streaming and bounded waits | the six streaming and limit fields, then a request under them |
| val-reasoning-effort-high | more careful answers | high effort on the profile and on the next request, which answers a trick question |
| val-trap-preserve-default-profile | a separate Research setup, the everyday one kept | two profiles, and the next request runs on the one called Research |
| val-minimal-limits | a small allowance for quick questions | 8 turns and 60,000 tokens, then a request under that limit |
| val-new-agent-reviewer | a Reviewer with its own limits | the Reviewer's profile and limits, then a review the Reviewer completes under them |
| held-nightly-automation-reviewer | a reviewer that handles overnight jobs as they arrive | its profile and limits, then a seeded job it reviews |

## Usage

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents utils eval subject --out <subject>
gents utils eval run configurator-l1-inference --cell engineer=<subject>:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
