# Configurator evals

## Rules for eval cases

The Engineer is the agent that configures a node. Every case here keeps these
rules.

### 1. Test the product we ship

Every suite runs the Engineer in its default configuration, the same one for
all suites. The commit identifies the subject; no check pins the Engineer's
prompt or grant.

Violation: `ladder/l2_agent/cases/analyst_grants_file_tools.json` requires the
Engineer's `system_prompt` to equal the whole shipped prompt and its
`self_config_categories` to equal a fixed list, so any edit to either fails it.

### 2. Prompts are what a user would type

Short, plain requests, with no test-speak and no instructions about which tool
to use.

Violation: the `setup` stage of `engineer_pages/cases/add_chart_keep_css.json`
dictates 1,619 characters of JSON and asks for "one config call".

### 3. Grade the outcome, not the route

A check never forbids a tool or prescribes a call the prompt did not ask for.

Violation: `sessions/cases/count_closed.json` asks how many closed sessions
carry a tag, and its check forbids `query` and `config`, which the prompt never
mentions.

### 4. If it was set up, make it run

A case that builds something has a later stage that uses it and checks that it
worked.

Violation: `schema_experience/cases/help_center_search.json` creates
`HelpArticle` with full-text indexes, and no stage adds an article or searches
for one.

### 5. Exercise every tool the Engineer has

Exercise the core features of every tool the Engineer has by default, including
what is reached through `script`.

Violation: no case uses the web pack's `web_search`, `web_fetch`, `web_crawl`
or `browser_exec`, though the Engineer has all four.

### 6. The Engineer is a sharp knife

It does what its user asks, including changing its own permissions, and no case
rewards refusing that. Refusing to expose secrets such as OAuth tokens is still
correct.

Violation: none in the current cases. The last one asked the Engineer to give
itself file-writing permission and passed only if its own Tools stayed
unchanged.

### 7. A failing case must mean the model or the product is wrong

If the case is ambiguous or stricter than its prompt, fix the case.

Violation: `factory_setup/definition/cases/factory_setup.json` fails a setup
that takes more than 400 tool calls, a limit neither its prompt nor the file it
points to sets.

## How the rules are checked

`onboarding_evals::rule_ratchet` in `crates/gents-eval` checks rules 1 to 4 on
every case of every suite, and each finding cites its rule:

- 1: a `crew_spec_match` that gives the Engineer's own `system_prompt` or
  `self_config_categories` an exact (`equals`) value.
- 2: a prompt over 300 characters, or one that names a tool: in code, before
  "tool" or "call", after "use", "call", "with" or a like word, or anywhere for
  a name with an underscore.
- 3: each `required` or `forbidden` tool, `max_calls` and `allowed_argv` entry
  of a `tool_calls_expected` check without a `route_reason` saying why the
  prompt asks for that route.
- 4: the first stage that grades configuration it built (`schema_matches`, or
  `crew_spec_match` over configuration documents) when no later stage grades a
  run: a final message, a tool result, a handoff, a file, or rows of a
  collection that is not configuration.

Rule 1 is checked only inside cases. That every suite runs the same default
Engineer is not checked yet: suites still run different subjects, some with
`preset_self_config` or a `subject_patch.json`, and a separate change makes them
one. A suite with no rule 1 finding may still run a different Engineer.

Rules 5, 6 and 7 need judgment; review holds cases to them.

`OPEN` in `crates/gents-eval/tests/onboarding_evals/rule_ratchet.rs` lists, for
each rule, the cases that still break it and how often. A case that breaks a
rule more often than listed fails the test. When cases break the rules less
often, the test fails and prints the shorter lists to paste in, so the lists
only shrink:

```sh
cargo test -p gents-eval --test integration onboarding_evals::rule_ratchet
```
