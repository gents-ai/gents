# Eval-runner canary pack

The subject under evaluation in `crates/gents/tests/eval_runner_canary.rs`.

One behavior (`canary`), one context, and one `Tools` document that grants
read-only file access and no bash: an embedded trial shares the runner's host,
so the freeze refuses a pack that grants unrestricted bash.

`notes.txt` is declared so an eval definition can list it under
`fixtures.assets`; the runner materializes it into the trial workspace, where a
`Capture::File` finds it.

The behavior references the `primary` inference slot the manifest declares
(`gents:inference-slot:primary`), which is the only form a pack may use. The
runner binds that slot when it installs the pack into a trial home, to the
`InferenceProfile` the run froze for the cell — so the profile a trial runs
against is the run's choice, not the pack's.
