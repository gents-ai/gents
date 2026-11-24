# factory_setup_eval

Native configuration acceptance for the research desk. Execution acceptance is run separately by the user.

The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Subject fixtures

- `research-desk-brief`: `factory/kickoff.md`, the user's written brief for
  the desk, which the case copies into the workspace and the stage prompt
  asks the Engineer to read. A user describes what they want built in a
  file in the Engineer's workspace.
