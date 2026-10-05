# Change unit pack

This pack runs one operator-authored `ChangeUnitWork` through a native Gents workspace callback, an exact-path writer, an independent read-only reviewer, and a host-owned serial integration. The review and closure preserve the writer receipt ID and seal hash. Rejected reviews produce a terminal result and never wake integration.

## Install

```sh
GENTS_CHANGE_UNIT_ROOT=/absolute/path/to/repository \
  gents pack install ./packs/gents/change_unit --home <home> \
    --inference-slot writer=<profile-id> \
    --inference-slot reviewer=<profile-id> \
    --inference-slot integrator=<profile-id>
```

The repository root is an operator-configured `RepositoryPlacement`; it must be a clean Git checkout. Create a work-unit document with the pinned repository ID and base commit, a unique work-unit ID beginning `change-unit:`, a unique branch, a concise title and instructions, and an `owned_files` JSON string containing the complete relative file-path array. The prefix isolates this pack's event subscriptions from other workspace receipts. `handoff_id` and `caused_by_correlation` must equal the work-unit ID. Supply the originating `reply_session_id` and a positive `attempt` so terminal outcomes retain supervision provenance. Retry bounds belong to the submitting supervisor; each retry needs a fresh unit/workspace identity. `caused_by_correlation` should equal the work-unit ID so later receipts and results correlate to the unit.

```sh
gents document create ChangeUnitWork --home <home> --json \
  '{"work_unit_id":"change-unit:change-001","repository_id":"change-unit-repository","base_sha":"<commit-sha>","branch":"change-001","title":"...","instructions":"...","owned_files":"[\"packs/example/file.md\"]","caused_by_correlation":"change-unit:change-001","handoff_id":"change-unit:change-001","reply_session_id":"<supervisor-session>","attempt":1,"status":"ready"}'
```

A successful run ends with `ChangeUnitResult.status=integrated`; a rejected review ends with `status=rejected`. Create a new work unit to try a revised request. The pack does not create a planning or repository-maintenance factory. Workspace creation and integration failures remain visible in Gents' durable callback invocation and request/workspace records.

The pack keeps atomic tasks in fresh sessions. Integration recording consumes the completed integrator FireOutcome and matches its exact request receipt. Configure an existing supervisor-owned Task and Trigger separately to deliver terminal outcomes into the originating requester-scoped session; durable results alone do not wake a supervisor.
