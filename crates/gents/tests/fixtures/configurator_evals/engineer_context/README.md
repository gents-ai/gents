# engineer_context

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, patched by `subject_patch.json` to cap one `read_file`
result at 4000 characters so a case can hold a file longer than one read.

Cases cover `context_budget`, the stubs that replace an earlier request's tool
outputs (recovered with the `sessions` output read once the file they came
from is gone) and reading past the read cap. No case forces a compaction.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-context`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
