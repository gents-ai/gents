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

## Retargeting a pack

Document JSON supports `${VAR}` and `${VAR:-default}`. The checked-in packs
default to the shared DeepSeek box, so they run as authored, and any run can be
retargeted without editing tracked config — which is how one pack is compared
across models:

```bash
GENTS_EXP_MODEL=some-other-model gents config apply --root experiments/<pack> ...
GENTS_EXP_ENDPOINT=http://127.0.0.1:8000/v1 gents server --apply-root experiments/<pack> ...
```

A `${VAR}` with no default and no value set is an error naming the variable —
it never silently becomes an empty string. Use `$$` for a literal `$`.
Interpolation applies to document JSON only; `.md` sidecars keep their runtime
`{{ }}` templates untouched.

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
