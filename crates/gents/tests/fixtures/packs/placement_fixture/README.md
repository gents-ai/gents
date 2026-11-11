# placement_fixture

A documents pack with three slot Agents whose repository placement declares two of them as readers.

## Configuration

Three tool-less agents (`fixture-writer`, `fixture-reviewer`, `fixture-integrator`) bound to the `worker` slot. The placement `placement-fixture-repository` reads its host path from `PLACEMENT_FIXTURE_ROOT` and declares `fixture-writer` and `fixture-reviewer` as readers.

## Usage

Installed by the pack installation tests that check declared placement readers.
