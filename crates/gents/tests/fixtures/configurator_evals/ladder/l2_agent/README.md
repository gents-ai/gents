# configurator-l2-agent

## Configuration

Draft component eval of the configurator ladder. The Engineer creates an agent with its own context and a Tools document with specific grants; the resolved tools must be exactly those, and the Engineer's own configuration unchanged. 
The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). Checks grade stored documents and runtime rows; model
quality is graded data, not a gate.

## Usage

```sh
gents config apply --root <this directory> --bind-node-did home --home <served home>
gents utils eval subject --out <subject>
gents utils eval run configurator-l2-agent --cell engineer=<subject>:engineer \
  --profile engineer=<profile without execution_id> --split train --trials 1 --home <served home>
```
