# Backend fingerprint credential remediation

Versions before PR #1382 used the backend's Debug representation as admission
configuration identity. When an inline API key was configured, that representation
included the key. It was copied to `InferenceCall.backend_config_fingerprint` and
could appear in timeline and adapter exports. Environment-variable references did
not embed the resolved environment value in that representation.

PR #1382 stopped new plaintext writes but used an unkeyed SHA-256 digest containing
the inline key. With the remaining inputs known, that digest allows offline
confirmation of candidate keys. Neither change erases prior data.

Current admission fingerprints use HMAC-SHA-256 with a fresh random 256-bit
process-private key. The key is never stored with call records, serialized, or
exported. The fingerprint remains stable within the process, so metadata edits
reuse a controller and credential or queue-capacity changes replace it. A restart
changes the fingerprint: it is runtime-scoped attribution, not a cross-host or
cross-restart content identifier. Cryptographic collision resistance and host
memory confidentiality are assumptions, not claims of the Lean registry model.

Timeline construction exports only the current `hmac-sha256:process-v1:` format
with a 64-character hexadecimal digest. Older Debug values, unkeyed hashes and
malformed values are omitted. This prevents ordinary timeline/adapter export from
redisclosing old values; it does not restrict administrative raw database reads.

Operators with affected inline-key configurations should rotate or revoke those
credentials at their provider. Updating a fingerprint column cannot invalidate a
credential already copied elsewhere. Do not paste original fingerprint values into
diagnostics, tickets or repair logs.

Historical repair is tracked in [#1394](https://github.com/gents-ai/gents/issues/1394).
It must cover current documents and separately account for DefraDB revision
history, P2P replicas (including offline peers), backups, and exported artifacts.
A current-row update must not be described as erasing those copies. No automatic
historical database mutation is performed by this runtime change. Repair should
be resumable and idempotent, report identifiers/counts only, and preserve call
lifecycle, attribution, and usage data unrelated to the fingerprint.
