# Full Engineer eval subject

## Configuration

The Engineer as `gents init --engineer` provisions it: the shipped prompt, the
Setup self-config grant, host files under the trial root, agent tools with its
own target, the built-ins init enables, the query tool and the
`engineer-mailbox` surface. Host bash is off because embedded eval isolation
refuses it. A definition on this subject lists every core pack in
`subject.core_packs`, and each trial installs them as init does.

## Usage

Name it as a suite's `subject` in its `suite.json`; a suite that needs a
variation adds `subject_patch.json` beside its definition.
