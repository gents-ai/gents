# configurator-l4-automation

Draft component eval of the configurator ladder. The Engineer configures an EventSource, a Trigger (parallel, queued_serial, or into an existing session via session_id_template) and a Task with templates and emit_outcome; a seeded document must produce a TriggerFire, a request and a FireOutcome in the right session. The subject is
`../engineer_subject`. Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents eval run configurator-l4-automation --cell engineer=<ladder>/engineer_subject:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
