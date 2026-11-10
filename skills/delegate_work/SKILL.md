---
name: delegate-work
description: Load before a long build, or when parts of a job can run on their own. When and how to use helpers.
---

# Delegate work

Every call resends your whole context, so a long session gets slow and expensive. A helper starts clean and comes back with a short answer.

## Hand off

- A part with clear inputs and a clear finish: the rules plugin, the page, an import.
- Work that takes a lot of reading and little judgment.
- Checking finished work. A helper that didn't build it reads `ACCEPTANCE.md` and tries each line.

## Keep

- The plan, the data model and what goes in the pack.
- Shared documents: one writer per document.
- Fitting the parts together and the final report.
- Anything that takes only a few calls.

## Brief a helper

It sees only what you send. Tell it:

- The goal, and what finished means.
- Which files to read. Give paths; don't paste them.
- Which skill to load.
- What not to touch.
- What to report: what it did, what it checked, what failed.

Start one with `agent_new`; the `engineer` target is a copy of you. For a narrower helper, create an agent and an agent target and add it to your own Tools, which applies from your next request. Tell the user what you added. Run independent helpers at the same time, and never let two write the same file. When one reports, confirm the result with one read or run before building on it.
