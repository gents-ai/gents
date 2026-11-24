# engineer_sessions

## Configuration

Session investigation eval definition over ACP-visible sessions. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Subject fixtures

- `partner-agent`: a second agent on the node, `Partner`, whose sessions the
  Engineer investigates. It shares the Engineer's Context and runs on its
  inference slot. A user's node runs other agents whose work the Engineer is
  asked about.

## Usage

Install the definition with `gents config apply --root <this directory>`. Run `scripts/evals/run-ladder.sh sessions` from the repository root.
