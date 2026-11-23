# configurator-l4-automation

## Configuration

Draft component eval of the configurator ladder. The Engineer configures an EventSource or a Schedule, a Trigger (parallel, queued_serial, or into an existing session via session_id_template) and a Task with templates and emit_outcome, and maintains them in place (pause, retime, suspend, resume, outcome records); a seeded document or a running schedule must produce TriggerFires, requests and FireOutcomes in the right session. The subject is
`../engineer_subject`. Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

## Usage

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents utils eval run configurator-l4-automation --cell engineer=<ladder>/engineer_subject:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
