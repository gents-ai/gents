# configurator-l5-agents-tools

Draft component eval of the configurator ladder. The Engineer creates a AgentTarget and selects it for agent_new/agent_message; a delegated result must return to the caller's session. The subject is
`../engineer_subject`. Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents eval run configurator-l5-agents-tools --cell engineer=<ladder>/engineer_subject:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
