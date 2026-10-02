# dependent_fixture

A minimal documents pack whose manifest depends on `fixture/review_graph`.
Used by dependency-resolution tests that need a pack with a real, named
dependency rather than an empty list.

## Configuration

One tool-less behavior (`fixture-dependent`) bound to the `worker` slot, and
one manifest dependency: `fixture/review_graph`. No schema, event source or
trigger of its own.

## Usage

Installed by dependency-resolution tests that need a pack whose install must
also resolve and install a named dependency.
