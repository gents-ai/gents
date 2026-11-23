# Canonical output maps

`canonical-output-map-{execution,projection,native,tools,control}.json` map
every constructor of the canonical output vocabularies (the `INVENTORIES` in
`.github/scripts/check-canonical-output-map.py`: the gate operations, session
composition trace, execution-lease, tool-call, retry, session-queue and
write-gate actions) to the Lean declarations that model it, its emitted
conformance groups, and the native owner and consumer paths.

- Each constructor is mapped exactly once, and every model symbol, emitted
  group and path a row names exists.
- `fixture_status` says how far the model exports the slice:
  `concrete_inputs` (an input/expectation boundary is emitted), `summary_only`
  (a witness or count is emitted, too little to drive the native operation) or
  `not_exported`. `native_status` is `pending` or `not_applicable`; a map never
  claims native coverage, which only the consumer scan and the coverage ledger
  establish.

`lake build` in `crates/gents/proofs` runs the check (target
`canonicalOutputMap`); `python3 .github/scripts/test-canonical-output-map.py`
tests the checker.
