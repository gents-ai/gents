# Experiment packs

Each subdirectory is a **self-contained pack**: pack-scoped `schemas/`, desired
state (including `DatastoreToolSurface`), and a README.

Use a **dedicated home** for experiments. The pack rebinds to whatever agent
the home owns, so applying it to a working home mixes experiment config into
that agent.

```bash
# Preferred: server applies the pack after ready (in-process schemas + config)
gents server --home <home> --apply-root experiments/<pack>

# Or apply against a running server / home
gents config apply --root experiments/<pack> --home <home> \
  --bind-agent-did home --force-rebind-concrete-did
```

> **`--apply-prune` deletes config the pack does not declare.** It makes the
> pack the *complete* desired state for the home's agent, so that agent's
> other behaviors, tool selections, skills, surfaces, and their reachable
> tasks/schedules/triggers are removed. Only pass it on a home dedicated to
> the pack; never on a home you use for anything else.

When `<pack>/schemas/` exists, **apply registers those SDL/patches first**,
then agent config (surfaces → selections → behaviors → tasks/triggers). Packs
do not touch product baseline schemas.

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
