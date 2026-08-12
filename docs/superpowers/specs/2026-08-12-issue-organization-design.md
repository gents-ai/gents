# Issue organization: program milestones

**Date:** 2026-08-12
**Status:** Approved design, not yet implemented
**Scope:** `source-inc/gents` issue tracker organization. No runtime code.

## Problem

127 open issues carry a two-axis label taxonomy defined in #839: eight `cluster:` labels
(architectural home) and four `roadmap:` labels (planning horizon). Three things have broken.

**1. The taxonomy decayed at the head of the backlog.** #839 requires exactly one `cluster:`
and exactly one `roadmap:` label per ordinary open issue. In practice 31 issues carry no
cluster label and 33 carry no roadmap label, and they are overwhelmingly the newest — almost
everything filed since #839 was written is untriaged. Nothing enforced the rule, so it decayed
silently.

**2. The largest coherent body of work has no home.** #1063 (schema architecture audit)
produced PR #1065, which was closed and decomposed into slices #1071–#1079 — seven P0s and a
P1 covering intent/authority separation, lease-epoch fencing, append-only transcript facts, and
exact-version provenance binding. With #1064, #1044, #1045, and #1048 that is 13 issues
carrying zero labels, invisible to every existing query. It is not expressible as a cluster: it
spans five of them.

**3. The cluster axis cannot express cross-cutting programs.** The iOS push (#890–#896) is
smeared across four clusters, and nine issues carry two cluster labels purely to express
membership in a campaign — already violating #839's "exactly one cluster" rule. The rule broke
because the taxonomy could not say what people needed to say.

Meanwhile #839 encodes five programs as ordered prose lists inside an issue body, where no
query can reach them, and its front half describes the Gents cutover as the active program —
work that is now complete except for #811.

## Decisions

Three decisions frame this design.

1. **A milestone represents a program of work, not a release or an iteration.** It has a
   definition of done and closes when the invariant holds. No due dates, so nothing goes stale.
2. **Milestones replace the `cluster:` axis** rather than sitting alongside it. Five of the
   nine programs were near-isomorphic to a cluster; maintaining both would mean two labels
   saying nearly the same thing while the genuinely cross-cutting programs stayed unexpressible.
3. **No GitHub Project.** Of #839's five stated trigger conditions, only "label queries can no
   longer answer a concrete planning question" fires — specifically, ordering. That is solved by
   putting the ordered critical path in each milestone's description, adjacent to the issues,
   rather than by adopting a second status system to reconcile. With 2 of 127 issues assigned
   (#696, #893) and 5 issues in `roadmap: now`, a Project would be pure overhead. Two assignees
   is well below the three-operator threshold #839 sets for revisiting.

## The two axes

| Question | Mechanism | Rule |
|---|---|---|
| What body of work does this belong to? | Milestone | At most one. Absent means backlog. |
| How soon? | `roadmap:` label | Exactly one, on every open issue. |

Type labels (`bug`, `enhancement`, `design`, `lean`, `runtime`, `cli`, `ui`, `audit`,
`documentation`, `research`, `blocked`, `meta`, `icebox`, `adapter-interop`) are unchanged.

The two axes are independent. An issue may be `roadmap: now` with no milestone (#833, #986),
or hold a milestone while sitting in `roadmap: later`.

### Accepted loss: there is no architectural-home axis

A program and an architectural area are orthogonal, and deleting `cluster:` deletes the only
axis that recorded area. The type labels do not substitute: `runtime` sits on 50 issues and
there is no equivalent for network, context, provider, or control plane. Two query classes are
therefore given up deliberately:

- **Cross-program subsystem queries.** "Show me every network/fleet issue" spans the fleet
  milestone, iOS (#893), quality-ci (#975, #1023), and unmilestoned work. After deletion this
  is a full-text search, not a label query.
- **Area for the 39 unmilestoned issues.** They keep type labels only. `quality-ci` covers 21
  of them; the other 18 have no area metadata at all.

This is accepted rather than mitigated. Reintroducing an `area:` axis would recreate the
cluster labels under a new name — the redundancy this design exists to remove — and it is the
axis that already decayed once. Labels are additive and backfillable, so if a concrete planning
question turns out to need area, adding `area:` later costs one `gh` sweep. The trigger for
revisiting is a *specific* question that cannot be answered, not general discomfort.

## Milestones

All eight are closable. Each milestone description carries its parent issue, its definition of
done, and its ordered critical path. Issue counts are open issues as of 2026-08-12.

### 1. Authority and provenance hardening — 13

Parent: #1063.

> **Done when** intent and execution authority are separate documents, every provider send and
> tool fact binds an exact `_docID` + composite CID, and no writer can forge or silently
> rewrite a fact it does not own.

Order: #1071 → #1073 → #1075 → #1079 → #1072 / #1074 → #1077 → #1064 → #1044 / #1045 / #1048 → #1078

Issues: 1063, 1064, 1071, 1072, 1073, 1074, 1075, 1077, 1078, 1079, 1044, 1045, 1048

### 2. Fleet convergence and P2P durability — 14

> **Done when** a fresh 19-node control mesh converges without runaway pending DAGs, and a
> failed push of a stable document retries instead of leaving it permanently missing on the peer.

Order: #630 / #696 / #798 treated as one diagnosis → #1036 → #977 → #1049 → #987 → remainder

Issues: 630, 696, 798, 977, 987, 938, 678, 679, 660, 606, 1049, 1036, 366, 673

### 3. Multi-agent coordination — 12

Parent: #832. Architecture: #378, #835.

> **Done when** a parent has one descendant graph across behaviors, await modes, deployments,
> and workflow roles; children have enforceable budgets; and a composed run is losslessly
> inspectable and continuation-safe.

Order: #836 → #835 → #838 → #834 → #734

Issues: 378, 832, 834, 835, 836, 838, 734, 937, 1096, 564, 868, 728

### 4. Long-context correctness — 15

> **Done when** token estimation is model-aware and centralized, compaction budgets are
> coherent and validated with a defined failure path, and trimmed evidence is recoverable.

Order: #718 → #719 → #717 → #716 → #722, then #1012 / #1025 / #720 / #1009

Issues: 716, 717, 718, 719, 720, 722, 723, 725, 1009, 1012, 1025, 391, 523, 652, 1054

### 5. Durable trace, attribution and trust — 12

> **Done when** every operational output carries generation and configuration attribution, the
> run timeline projects the complete durable request record, and export redaction is bound to
> ACP rather than caller choice.

Order: #842 → #845 → #846 → #841 → #843 → #844. #847 is decomposed before promotion and
reconciled against #461 and #539 rather than building parallel authorization paths.

Issues: 841, 842, 843, 844, 845, 846, 847, 461, 539, 749, 882, 621

### 6. Provider fidelity and rig removal — 14

> **Done when** rig-core is gone, the provider wire contract has a support matrix and replay
> fixtures, and token usage is complete and attributable by inference call, request, and session.

Order: #991 → #509 / #726 → #438 → #439

Issues: 438, 439, 498, 509, 514, 540, 708, 726, 737, 741, 748, 991, 897, 1047

### 7. iOS hardening — 7

> **Done when** iOS has a distribution path beyond development export, identity keys and the
> node directory are protected, replication and streaming survive app suspend, and CI covers the
> simulator build and E2E lane.

Order: #892 → #896 / #894 → #893 → #895 → #891 → #890

Issues: 890, 891, 892, 893, 894, 895, 896

Closing this milestone dissolves the double-labeling on #893, #895, and #896.

### 8. Gents cutover — 1

> **Done when** #811 closes.

Issues: 811

## Unmilestoned work (39)

`no:milestone` is a legitimate backlog query, not a triage failure.

**Quality and CI (21)** — retain the `quality-ci` label:
52, 330, 730, 743, 750, 800, 802, 803, 816, 874, 884, 975, 989, 1023, 1035, 1041, 1082, 1083,
1084, 1085, 1093.

This is a standing lane; issues arrive forever and it would never close. A never-closing
milestone produces a progress bar that never fills, which is how milestone hygiene dies.
"Flaky tests are defects" is unaffected — this governs scheduling, not legitimacy.

**Everything else (18)** — desktop and platform work (527, 543, 580, 608, 647, 705, 742, 849,
986), one-offs (700, 732, 739, 833, 899, 980, 1086, 1090), and #839 itself.

## Label migration

**Delete seven cluster labels:** `cluster: context-memory`, `cluster: network-fleet`,
`cluster: subagents-workflows`, `cluster: inference-provider`, `cluster: tools-control-plane`,
`cluster: clients-platform`, `cluster: gents-cutover`.

**Rename `cluster: quality-ci` to `quality-ci`.** Renaming preserves existing issue
associations and saved queries; deleting and recreating would not. The name is kept over
`flaky` because the set includes CI infrastructure (#816) and test-harness migration (#1086),
which `flaky` would misname.

**Backfill `quality-ci`** onto the eight members that carry no label today: 52, 884, 1035,
1041, 1082, 1083, 1084, 1085.

**Backfill `roadmap:`** onto the 33 issues missing it, per the manifest below. Every issue gets
a named horizon so the implementer makes no prioritization decisions.

#### roadmap: now (2)

| Issue | Reason |
|---|---|
| #1071 | Root of the #1063 family. Intent/execution separation is the refactor every other P0 in that milestone builds on; starting anywhere else means rework. |
| #1093 | Agent boot overruns the conformance ready budget on CI after #1087. Blocks the current SHA, which is #839's stated bar for `now`. |

This moves `roadmap: now` from 5 to 7, within the repository limit set below.

#### roadmap: next (14)

| Issues | Reason |
|---|---|
| #1063, #1064, #1072, #1073, #1074, #1075, #1077, #1079 | The remaining #1063 P0 stack and its parent. Committed, ordered in the milestone description, released into `now` one at a time as #1071 lands. |
| #1044, #1045, #1048 | Concrete injection and ACP holes — unvalidated identifier splicing and a source-doc read running as node root. Small, self-contained, and security-relevant. |
| #1036, #1049 | Fleet convergence entry points: the #984 replicator/reconciler diagnosis and the push-retry gap that silently loses documents. |
| #884 | `test:live:chat` failing on main. Same class as #803, which is already `next`. |

#### roadmap: later (17)

| Issues | Reason |
|---|---|
| #52, #330, #975, #1035, #1041, #1082, #1083, #1084, #1085 | Flakes that do not currently block the active SHA. Per #839 they move to `now` when they block it or reproduce deterministically. |
| #1086, #1090 | Test-harness migration and declarative datastore packs. Neither gates a program. |
| #897, #1047, #1054, #1096 | Milestone work not on any critical path: admission-controller reactivation, the Lean CodexShim staleness window, title-generation alignment, EventTrigger run scope. |
| #980 | Design-only MCP tool-injection exploration. |
| #1078 | The one P1 in the #1063 family. Versioned archive, restore, legal hold, and purge receipts sit last in that milestone's order and gate nothing ahead of them. |

Counts: 2 + 14 + 17 = 33.

**Add `needs-triage`** for the enforcement action below.

## Enforcement

The taxonomy decayed once because nothing checked it. Re-labelling without enforcement buys
roughly one quarter.

A GitHub Action, `.github/workflows/issue-triage-hygiene.yml`.

**One reconcile function, called identically from every trigger.** For an open issue, count its
`roadmap:` labels:

| Count | Action |
|---|---|
| 1 | Remove `needs-triage` if present. No comment. |
| 0 | Add `needs-triage` if absent. No comment. |
| ≥2 | Add `needs-triage` if absent. Comment naming the conflicting labels, unless a prior conflict comment already names the same set. |

**Triggers:** `issues: [opened, reopened, labeled, unlabeled]`, `schedule` (daily), and
`workflow_dispatch`. Event runs reconcile the single subject issue; scheduled and dispatch runs
sweep all open issues.

Three properties this shape buys, each fixing a way the naive version fails:

1. **`opened` and `reopened` run the full reconcile, not a blind `needs-triage` add.** A
   reopened issue that already carries exactly one `roadmap:` label must come out clean on that
   run. This is not merely tidier — it is required for correctness, because label mutations made
   with the default `GITHUB_TOKEN` do not trigger further workflow runs. A blind add would rely
   on a `labeled` event that never fires, leaving the issue wrongly flagged until the next daily
   sweep.
2. **Mutations happen only on state change.** Never re-add a present label or remove an absent
   one. Redundant writes generate timeline noise and further `labeled`/`unlabeled` events from
   any non-`GITHUB_TOKEN` actor, which can ping-pong.
3. **Conflict comments are deduplicated** by an HTML-comment marker embedding the sorted
   conflicting label set. Re-running against an unchanged conflict must stay silent; a *changed*
   conflict set posts once.

Permissions: `issues: write` only. Concurrency is grouped per issue number so two events on the
same issue cannot interleave.

**Scope.** The action enforces the `roadmap:` rule and nothing else. Milestone assignment is a
judgment call and is deliberately not enforced — at most one milestone is a GitHub invariant
already, and "no milestone" is legal. The action never closes, assigns, reprioritizes, or
milestones an issue.

**Exemption.** Issue #839 only, by number. An earlier draft exempted the `meta` label, which
would have silently exempted every future `meta` issue and contradicted the "exactly one
`roadmap:` label on every open issue" rule. Pull requests are out of scope: the `issues` event
does not fire for them.

## #839 rewrite

#839 keeps horizon semantics and the Project revisit trigger. It loses the completed cutover
narrative and the five prose program sections, whose content moves into milestone descriptions.
Retitle from "Roadmap: Gents cutover and post-cutover runtime programs" to reflect that it is
now operating rules rather than a program list.

**The work-in-progress rules must be rewritten, not kept.** #839 currently reads "at most one
implementation issue per cluster is active" and "at most three implementation issues are active
across the repository." The first rule references an axis this design deletes. Replace both with:

- at most one implementation issue per **milestone** is active;
- at most **ten** issues sit in `roadmap: now` repository-wide;
- design and proof work may overlap implementation only when it feeds the next executable slice;
- moving an issue into `now` should move something else out unless it is a production invariant
  or release blocker.

The ten-issue repository limit replaces the old three-issue implementation limit because `now`
holds non-implementation work too — #816 is CI infrastructure, #833 is a bug, #839 is meta — and
the old rule silently counted only some of it. Ten also matches the ">10 simultaneously in
progress" threshold #839 already sets for creating a Project, so the two rules can no longer
disagree. The backfill above lands `now` at 7.

Retitling softens #839's role as the canonical "the roadmap" link. Anything referencing it by
title rather than number needs updating; the issue number and URL are unchanged.

The "Why there is no GitHub Project yet" section is retained and updated: the ordering gap it
anticipated is now answered by milestone descriptions, and the remaining trigger conditions
(three or more people holding assigned issues, more than ten in `roadmap: now`, cross-repo
scheduling with `defradb.rs`) stand as the explicit revisit gate.

## Explicit judgment calls

**#716 and #722 go to Long-context correctness, not Multi-agent coordination.** #839 lists
both under program A *and* program B. Milestones are exclusive, and both issues are compaction
and tool-result-truncation work, so Long-context owns them. Multi-agent coordination therefore
has a dependency it cannot see from its own milestone view; the coordination milestone
description must name #716 and #722 as external prerequisites.

**Flaky tests take no milestone even when they are a program's own evidence.** #975 and #1023
are P2P flakes that could be read as fleet-convergence evidence. Admitting them would make the
rule a judgment call per issue and the boundary would erode. They stay in `quality-ci`.

**#890 is in iOS hardening despite carrying `quality-ci`.** It is missing CI coverage for a
platform, not a flaky test, and it is part of what "iOS is shippable" means.

## Out of scope

Creating a GitHub Project; adding due dates; assigning owners; re-prioritizing any issue's
horizon beyond the backfill above; closing stale issues; changing type labels; any runtime,
proof, or CLI code.

## Success criteria

1. Eight milestones exist, each with a description carrying parent, definition of done, and
   ordered critical path.
2. All 88 mapped issues carry their milestone; the other 39 carry none.
3. Every open issue except #839 carries exactly one `roadmap:` label, matching the backfill
   manifest. `roadmap: now` holds 7.
4. The seven cluster labels are deleted; `quality-ci` exists un-prefixed on 22 issues — the
   21 unmilestoned ones plus #890, which keeps it inside the iOS milestone.
5. `is:open no:milestone -label:quality-ci` returns the 18-issue backlog and nothing surprising.
6. A new issue filed with no `roadmap:` label acquires `needs-triage` automatically; an issue
   reopened with exactly one `roadmap:` label does not, on that same run.
7. #839 no longer duplicates any program's contents.
