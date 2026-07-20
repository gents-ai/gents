# DefraDB store-upgrade fixture

`v0612_populated_rocksdb.tar.zst.b64` contains only the RocksDB directory from
a normal `defra-agent init` run using v0.6.12
(`b7a4b4ea5fdedab14e2269250e0d6f0f89c4f254`). It has the complete runtime
schema set plus the synthetic principal, backend, behavior, inference profile,
and tool selections created by init.

The fixture uses a loopback inference endpoint, no API key, and a synthetic DID
with no external authority. Identity-key and init metadata files are excluded.
The e2e test materializes this store, opens it through the current pinned
DefraDB, runs the existing agent runtime migration pipeline, verifies durable
configuration and legacy commit-history lookup, writes an update, and reopens
the same store idempotently.

To inspect the archive:

```sh
base64 --decode < v0612_populated_rocksdb.tar.zst.b64 |
  zstd --decompress |
  tar -tf -
```
