# `gents eval init`: the authoring wizard

Approved section by section on 2026-09-23. Umbrella: `2026-09-21-eval-and-optimization-umbrella.md`.
Contract: `2026-09-21-eval-core-contract-design.md`. Runner: `2026-09-21-eval-runner-design.md`.
CLI: `2026-09-22-eval-report-and-cli-design.md`. Lands as one PR on `feat/eval-cli` (#1658).

## Goal

Let an operator go from "this behavior in this pack" to a validated eval definition pack on disk
through an interview with a model, the way `claude plugin eval init` does for a plugin, without
the author ever touching files, documents or tools. Optionally pilot the draft against the subject
once and let the author revise from the evidence.

## Decisions

| Question | Decision | Why |
|---|---|---|
| Wizard depth | Model drafting only; no deterministic scaffold | The value is in the drafted cases; a skeleton without cases is what copying the monitor pack already gives |
| Where the author runs | An `AgentRequest` on the operator's home, on a dedicated session, behavior `eval-author` from a built-in `eval_author` pack | Simplest plumbing; the operator's login and default profile work; the transcript is an ordinary session |
| Interaction | Interview first, then draft; the same turn loop as `gents chat` on a fixed session | A brief-only path produces drafts only as good as the brief; the interview is the point |
| Output path | The author answers with JSON; the CLI validates and writes | The author needs no write grants; nothing reaches disk or live configuration unvalidated |
| Pilot | Opt-in `--pilot`: one run at one trial per case on all splits, one revision round | Evidence before freezing cases, without forcing spend on every init |
| Stack placement | On `feat/eval-cli`, registry-agnostic | The mailbox checks are on the held side branch; the wizard renders whatever the registry ships and stays useful as the registry grows |

## 1. The command and its flow

```
gents eval init <subject> [--behavior <id>] --out <dir> [--definition-id <id>]
                [--profile <profile_id>] [--pilot] [--yes] [--force] [--home | --graphql]
```

- `<subject>` is a pack name or a pack directory, resolved as `eval run --cell` resolves one.
  `--behavior` selects the behavior when the pack declares more than one; a single behavior is
  implied; none or an unknown id is a refusal.
- The command needs the operator's home to be served: the author's requests are claimed and run
  by that runtime, exactly as `gents chat` turns are. It resolves the GraphQL endpoint the way
  `gents chat` does (`--graphql`, else the home's runtime state) and refuses with "start
  `gents server` for this home" when nothing answers. It installs the built-in `eval_author` pack
  into that home once, idempotently (re-applying the same documents changes nothing), binding its
  single inference slot `author` to `--profile` or the home's default profile. The pack is one
  behavior `eval-author` with a context and no tools.
- It creates a fresh `AgentSession`, submits the first turn with `behavior_id = eval-author`: the
  subject dossier (section 2), the check catalog and the authoring contract (section 3). The
  author's first reply is its questions.
- A terminal loop follows, using the chat turn function on that session: the operator answers,
  the author asks or drafts. A reply that contains exactly one fenced `json` block is a draft and
  enters validation (section 4). Validation failures are sent back as the next turn; the author
  revises. Three validation rounds, then exit 1 with the last messages and nothing written.
- On success the pack is written under `--out`, the case table and the next commands are printed,
  and the session id is printed so `gents chat --session-id` can continue.
- With `--pilot` the command continues into section 5.
- A non-terminal stdin is refused: this command is an interview. Ctrl-C ends the session and
  leaves what was already written.

## 2. The subject dossier

The author reads nothing itself. The CLI renders the subject into the first turn from `read_pack`
and `load_pack_config` with an empty interpolation environment, so placeholders such as
`${GENTS_EVAL_WORKSPACE_ROOT}` appear as markers. The dossier holds, in order:

1. Identity: pack name, version, digest, the chosen behavior id and display name, the slot it binds.
2. The behavior's context: its system prompt verbatim.
3. The tool surface: for each tools document the behavior selects, host file mode, whether bash
   is granted, and every datastore surface entry (tool name, collection, fields, filters,
   description).
4. Tasks and triggers: each task's prompt template verbatim with the variables it expects.
5. Schemas: every `schemas/*.graphql` asset verbatim.
6. Fixture files the pack ships, by path.

Excluded on purpose: inference documents (packs declare none), anything in the operator home, and
the pack README (prose invites testing the prose). A dossier over 64 KB is refused, naming the
largest asset.

## 3. The check catalog and the authoring contract

`Check` gains `fn describe(&self) -> CheckDescription` with `summary`, `params_schema` (JSON
Schema for `params`), `reads` (capture kinds and fields the check consumes) and `reason_codes`
(code and one line each). `CheckRegistry::catalog()` renders every registered check to JSON. The
catalog feeds the author's first turn, the new read-only `gents eval checks [--json]`, and
validation step 2. Nothing in the wizard names a check.

The authoring contract is fixed text in the `eval_author` context:

- The vocabulary (definition, case, stage, capture, check), the case JSON shape, the three
  reducers, one worked example.
- The rules drafts are held to: checks from the catalog with schema-valid params; captures over
  collections and fields the dossier shows; all three splits populated with at least six
  validation cases unless the operator says otherwise; kebab-case unique case ids; stage
  deadlines default 600 s; acceptance tier only.
- The interview: what the behavior must get right, what would fool it, what it must never do,
  case count and split, banned words; one or two questions per turn; draft when answered or when
  the operator says "draft".
- The draft format: a fenced `json` block whose top level is `{"definition": {...}, "cases": [...]}`
  and nothing else in that turn.
- When the catalog cannot express a case, say so in prose and draft only what it can grade;
  never invent a check.

## 4. Validation and writing

1. Parsing: exactly one fenced `json` block, parseable, with `definition` and `cases`.
2. Assembly by the CLI: `definition_id` from the flag or the draft, `comparability_version: 1`,
   `subject: {kind: behavior, inference_slots: [<the chosen behavior's slot>]}`, cases inlined.
   The author sets neither subject nor version.
3. `EvalDefinition::validate`.
4. Catalog conformance: every check registered; params valid against its `params_schema`.
   Unknown name and schema violation are distinct messages naming case, stage and check.
5. Capture conformance: every documents capture names a collection from the dossier's datastore
   surfaces or schemas and only fields of that collection; every file glob is relative.
6. Split shape: all three splits present, validation at the interview floor (default six), no
   duplicate case id.
7. Loader round trip: write to a temporary directory, load through the real pack loader with the
   sidecar rule, install the result into a scratch embedded home. The stack tip's loader has no
   eval-case sidecar hydration yet (the held side branch adds it), so this PR carries that
   loader change; the side branch drops its copy when it rebases.

Failure sends the messages back verbatim as the next turn, prefixed "The draft did not validate;
revise and reply with a new draft." Success moves the temporary directory to `--out` (refused if
it exists, unless `--force`): `manifest.json` (name from the definition id, version 1.0.0,
`kind: documents`, every asset listed, no inference slots), `pack_config.json` with
`agent_principal: {}` and the definition entry pointing at `./cases/<case_id>.json`, one sidecar
per case, and a generated `README.md` with the case table and the interview summary.

## 5. The pilot

With `--pilot`, after the pack lands: install it into the home as a directory pack, then
`runner::freeze` and `runner::run` with one cell `pilot=<subject>[:<behavior>]` on the named
profile, every split, `trials_per_case: 1`, `purpose: "pilot"`, run id
`<definition_id>-pilot-<unix ms>`, the embedded executor, Ctrl-C cancelling as for `eval run`.

`purpose: "pilot"` is recorded in the origin; `eval list` shows it; the exposure count skips
pilot runs, and `compare` refuses a pilot run unless `--include-pilot` is given. A pilot is a
look at cases that are about to change.

When the run ends, the report (`eval::report::build`) is folded into one turn: per case and
stage, each check's kind, score and reason code, plus captured rows for failing stages under a
fixed per-case budget, with the instruction that a failing check may mean a wrong case or a wrong
subject, and to reply with a full revised draft or "keep". A revised draft goes through section 4
and overwrites `--out`; exactly one revision round; the revised pack is not piloted again. The
README records the pilot run id and that a revision followed.

The command prints the case count and asks for confirmation before spending, unless `--yes`.

The pilot is not calibration: one trial per case cannot separate noise from a wrong case. Its job
is spec 3a's: matcher misses, empty versus absent rows, keyword survival.

## 6. Testing

- Dossier rendering against the canary pack fixture: golden text, placeholders as markers, the
  64 KB refusal.
- Catalog rendering: every builtin check describes itself; `params_schema` is a valid schema;
  `gents eval checks --json` round-trips.
- Validation: one hand-built draft per failure step, each refused with its message; a valid draft
  reaches `--out` with the exact file set; `--force` semantics.
- The interview loop over a scripted turn source (a trait the loop takes; production wires the
  chat turn function): questions, a bad draft, a good draft; the three-round limit.
- The pilot on `ScriptedExecutor` with scripted rows, as the canary does: the run is created with
  `purpose: "pilot"`, the digest turn contains the failing check, "keep" ends, a revised draft
  overwrites.
- `eval list` shows the purpose; `compare` and exposure exclude pilot runs.
- Live, ignored by default: one init against `eval_monitor` on the operator's profile.

## Out of scope, with a home

A deterministic scaffold (none planned). Brief-driven one-shot authoring for CI (revisit when a
second operator asks). Piloting the revised draft (the operator runs `eval run`). Drafting checks
that the registry lacks (spec 3b, WASM graders). The `llm_judge` development-tier check in drafts
(spec 6).
