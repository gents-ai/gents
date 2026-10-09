# configurator-l2-agent

Draft component eval of the configurator ladder. The Engineer creates an agent with its own context and a Tools document with specific grants; the resolved tools must be exactly those, and the Engineer's own configuration unchanged. The subject is
`../engineer_subject`. Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents eval run configurator-l2-agent --cell engineer=<ladder>/engineer_subject:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
