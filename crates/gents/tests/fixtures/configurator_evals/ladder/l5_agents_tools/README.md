# configurator-l5-agents-tools

## Configuration

Draft component eval of the configurator ladder. The Engineer creates a AgentTarget and selects it for agent_new/agent_message; a delegated result must return to the caller's session, an existing thread receives a message once, and agent_stop retires exactly the named session. 
The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

## Usage

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents utils eval subject --out <subject>
gents utils eval run configurator-l5-agents-tools --cell engineer=<subject>:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
