# Experiment packs

Each subdirectory is a **self-contained pack**: pack-scoped `schemas/`, desired
state (including `DatastoreToolSurface`), and a README.

```bash
gents config apply --root experiments/<pack> --home <home> \
  --bind-agent-did home --force-rebind-concrete-did
```

When `<pack>/schemas/` exists, **config apply registers those SDL/patches
first**, then applies agent config (surfaces → selections → behaviors →
tasks/triggers). Packs do not touch product baseline schemas.

## Packs

| Pack | What it shows |
| --- | --- |
| [`pipeline/`](pipeline/README.md) | **Canonical example** — job → finding create via surface → stage-2 |

## Model

| Concept | Mapping |
| --- | --- |
| Node | Task + behavior |
| Edge | EventTrigger `event_kind: created` only |
| Create tools | `DatastoreToolSurface` linked from `ToolSelection` |
| Kickoff | One GraphQL create of the pack’s seed collection |
