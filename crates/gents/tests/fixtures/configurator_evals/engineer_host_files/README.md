# engineer_host_files

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, as `gents init --engineer` leaves the Engineer. Each case
writes its input files into the trial workspace, where the Engineer's host file
tools are rooted, and grades the files the stage leaves there with
`captured_files_match`: inputs it must not change by their sha256, outputs by
their text or JSON values. Bash is off.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-host-files`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
