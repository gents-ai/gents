# engineer_pack_authoring

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). It holds the pack and pack-authority grants
and does all pack work through `config`. Cases put draft packs in the trial
workspace as fixture files and grade the pack directories the stage leaves,
the PackInstallation rows, the installed documents and schemas, and tool
receipts. No case compiles plugin source or reaches the registry.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-pack-authoring`;
held-out cases run only with `GENTS_EVAL_SPLITS=held_out`.
