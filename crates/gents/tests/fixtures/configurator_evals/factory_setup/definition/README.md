# factory_setup_eval

The research desk capstone: the Engineer configures a six-role desk from a
written brief, repairs what a review finds, then runs one brief request
through it.

The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Subject fixtures

- `research-desk-brief`: `factory/kickoff.md`, the user's written brief for
  the desk, which the case copies into the workspace and the stage prompt
  asks the Engineer to read. A user describes what they want built in a
  file in the Engineer's workspace.

## Stages

| Stage | User need | Graded by |
| --- | --- | --- |
| setup | the desk the brief describes, configured and not started | each role's Tools, record tools, limits, handoffs and the Engineer's delegates; the receipt in the mailbox; no request to a desk agent |
| repair | the same, after reading what the review found | the same checks, at acceptance tier |
| run | one brief request, two questions, taken through the desk | the request, two assignments, two findings, one draft and a review decision for that batch; at most one published brief |

The role and count checks read only the desk's own documents: the agents the
core packs install are left out of the agent captures by name, and event
sources are counted over the desk's five collections. The Engineer's delegates
are the six roles and its own default target.

`sources/` in the case's fixture files holds two short documents the Researcher
reads during the run.
