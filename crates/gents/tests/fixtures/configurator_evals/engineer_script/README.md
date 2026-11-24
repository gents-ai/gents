# engineer_script

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, as `gents init --engineer` leaves the Engineer: the
`gents/script` plugin is on its Tools with caller-tool access, so a script's
`tools.NAME(...)` calls run with the Engineer's own file, query and mailbox
tools. Calls a script makes are recorded as AgentToolCall rows whose
`plugin_parent_tool_call_doc_id` names the script call.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-script`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
