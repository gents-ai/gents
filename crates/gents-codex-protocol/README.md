# Gents Codex protocol

This crate owns the JSON-RPC wire types used by the Gents Codex compatibility
shim. The definitions are a snapshot of the Codex app-server protocol revision
recorded below, but intentionally exclude server-runtime helpers, schema
export, history reconstruction, command parsing, and other implementation-only
code.

Keeping the wire vocabulary here lets `gents` speak to the external Codex UI
without linking the upstream app server's runtime-oriented dependency surface.

## Updating the protocol

The current snapshot follows Codex revision
`c4e53d103c102f8d5201247adbc60bbddd47c88d`. When updating it, copy only the
serializable request, response, and notification types the shim needs; keep
runtime conversions and server helpers out of this crate. Then run:

```console
cargo test -p gents-codex-protocol
cargo test -p gents-cli --test cli_codex_shim
```

Add or update a wire fixture whenever a method name, enum spelling, tagged
representation, or nested JSON shape changes.
