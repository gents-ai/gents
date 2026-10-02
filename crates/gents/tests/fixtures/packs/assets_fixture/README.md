# assets_fixture

A test-only assets pack. It ships no configuration and installs no
documents; it exists so archive, store, remove and update tests exercise a
real pack directory instead of ad hoc bytes.

## Configuration

None. An `assets`-kind pack carries no `pack_config.json` and installs no
documents; it only declares files (`README.md`, `guide.md`,
`data/nested.json`).

## Usage

Packed and copied by archive, store and name-index tests that need a real
pack directory to pack, store, verify, release or rename; never installed
onto a runtime agent.
