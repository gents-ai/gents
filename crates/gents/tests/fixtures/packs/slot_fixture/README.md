# slot_fixture

A minimal documents pack with a single inference slot (`author`) bound to a
single, tool-less behavior (`fixture-author`). Used wherever a test needs a
real pack to install onto a named slot without any tool surface to satisfy.

## Configuration

One behavior (`fixture-author`) with no tools (`bash.mode: Off`, no
declared host) bound to the `author` slot. No schema, event source or
trigger.

## Usage

Installed by eval-author and proposer tests that need a minimal, real pack
to install onto a named inference slot.
