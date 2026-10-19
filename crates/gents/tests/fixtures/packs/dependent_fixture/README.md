# dependent_fixture

A minimal documents pack whose manifest depends on `fixture/dependee_fixture`.
Used by dependency-resolution tests that need a pack with a real, named
dependency rather than an empty list.

## Configuration

One tool-less agent (`fixture-dependent`) bound to the `worker` slot, and
one manifest dependency: `fixture/dependee_fixture`. No schema, event source or
trigger of its own.

## Usage

Installed by dependency-resolution tests that need a pack whose install must
also resolve and install a named dependency.
