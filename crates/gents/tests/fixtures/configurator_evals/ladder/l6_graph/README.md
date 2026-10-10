# configurator-l6-graph

Draft component eval of the configurator ladder. The Engineer wires graph nodes, templated prompts, inputs and outputs; a graph run must produce the expected output. The subject is
`../engineer_subject`. Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents eval run configurator-l6-graph --cell engineer=<ladder>/engineer_subject:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
