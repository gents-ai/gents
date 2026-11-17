# Hosted pack install review

The hosted install session serves the CLI and desktop from the runtime. Its
preview describes the writes the runtime plans to attempt, not a comparison
against existing document bodies. Human output prints this limitation, and the
JSON preview includes it in a `notes` list. A successful preview therefore does
not guarantee that applying the reviewed plan will avoid a document conflict.

A hosted conflict gives the `--overwrite` / `--keep` retry choices on the same
install. Local installs retain the local update/remove advice. Failure classes
are fixed and error chains are not client diagnostics: variable-derived Tools
ids, plugin coordinates and pins must not be quoted in typed refusal text.
Edited-document names can come from an earlier installation, so hosted Edited
refusals omit those names regardless of the new archive's contents.

This is a refusal-message guarantee, not a general secrecy policy for installed
identifiers: successful previews, receipts and installed documents still contain
rendered identifiers. Supply explicit install values deliberately; identifiers
must not be used as secret storage. The existing User install-value resolver can
also resolve runtime environment values. This change does not redesign that
resolver or redact successful install data.

Build identity retains its version, commit and core-pack digests. Registering it
for a non-install command does not hash the embedded pack archives. The runtime
resolves and caches the digests when an install handshake first asks, on a
blocking worker rather than on the asynchronous executor. The install compatibility
check is unchanged.


Legacy `POST /packs/install` failures now return `{error, outcome, class}`.
HTTP statuses and outcomes are unchanged; `error` is a class description or
provenance-checked typed text, never an internal error chain.
