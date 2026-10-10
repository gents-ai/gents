# configurator-l3-datastore

Draft component eval of the configurator ladder. The Engineer registers a schema, creates a DatastoreToolSurface for it, selects it in Tools and makes a successful call; all four must hold together. The subject is
`../engineer_subject`. Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents eval run configurator-l3-datastore --cell engineer=<ladder>/engineer_subject:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
