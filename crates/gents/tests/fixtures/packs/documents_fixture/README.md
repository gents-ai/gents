# documents_fixture

A pipeline-shaped documents pack: one `worker` slot, one tooled behavior with
a read-only host workspace, one schema, and one filtered event source. Used
by scenario, eval, proposer-refusal and `gents pack check` tests that need a
real, minimal documents pack rather than one of the official ones.

## Configuration

One behavior (`fixture-worker`) with a read-only host workspace rooted at
`${GENTS_FIXTURE_ROOT:-.}`, one schema (`FixtureJob`), one event source that
fires on a `FixtureJob` document with `status: "ready"`, and the trigger that
runs `fixture-worker-task` from it.

## Usage

Installed by scenario, eval and `gents pack check` tests that need a real,
minimal documents pack with a tool surface and an event-driven trigger to
exercise, rather than one of the official packs.
