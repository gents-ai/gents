# Eval author

The eval author is the model side of `gents eval init`. It is a single
behavior, `eval-author`, with a fixed system prompt and no tools: it cannot
read or write documents, files or configuration. It interviews an operator
about one subject behavior and drafts an eval definition — cases that will
later run against that subject and grade what it does.

`gents eval init` installs this pack into the operator's home once,
idempotently, and binds its one inference slot, `author`, to `--profile` or
the home's default profile. It then opens a fresh `AgentSession`, sends the
subject dossier and the check catalog as the first turn, and runs an
ordinary chat-turn loop between the operator and the author.

A reply that contains exactly one fenced `json` block is a draft. The CLI —
never the author — validates it: check names against the catalog, params
against each check's schema, captures against the subject's collections and
fields, split and case-id shape, and a full pack-loader round trip in a
scratch directory. Only a draft that survives every step is written under
`--out`. The author holds no write grants at any point; nothing reaches disk
or live configuration before validation passes.

See `agent_behaviors/eval_author/system_prompt.md` for the authoring
contract: the case vocabulary, the worked example, the seven rules drafts
are held to, the five interview questions, and the draft reply format.
