# configurator-l1-inference

Draft component eval of the configurator ladder. The Engineer creates an InferenceProfile and an InferenceExecution with explicit limits, binds them, and a later request runs on the new profile. The subject is
`../engineer_subject`. Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents eval run configurator-l1-inference --cell engineer=<ladder>/engineer_subject:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
