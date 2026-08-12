# Issue Organization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the decayed `cluster:` label axis with eight closable program milestones, backfill every open issue's `roadmap:` horizon, and add a GitHub Action that keeps the taxonomy from decaying again.

**Architecture:** Two phases. Phase A (Tasks 1–7) mutates GitHub issue state through `gh`, ordered so no metadata is destroyed before its replacement exists: snapshot → create milestones → assign → rename the one label we keep → backfill → only then delete. Phase B (Tasks 8–10) adds enforcement as a dependency-free Node script split into a pure `reconcile()` decision function (unit-tested with `node --test`) and a thin I/O runner, so the logic is testable without hitting the API.

**Tech Stack:** `gh` CLI, GitHub REST API, Node ESM (`.mjs`, no dependencies), `node --test`, GitHub Actions.

## Global Constraints

- Spec of record: `docs/superpowers/specs/2026-08-12-issue-organization-design.md`. Where this plan and the spec disagree, the spec wins — stop and reconcile.
- Repository: `source-inc/gents`. All `gh` commands run from the repo root.
- 127 open issues at plan time: 88 milestoned, 39 unmilestoned. Every count in this plan is a verification target, not a guess.
- **`cluster: quality-ci` is RENAMED, never deleted and recreated.** Renaming preserves issue associations; delete-and-recreate loses them irrecoverably.
- Cluster label deletion (Task 6) happens only after milestone assignment (Task 4) succeeds. Deleting first destroys the only record of grouping.
- The enforcement workflow (Task 10) lands only after the `roadmap:` backfill (Task 5), so it does not flag 33 issues on its first run.
- Node scripts use `node:` built-in imports only. No npm dependencies.
- The action never closes, assigns, reprioritizes, or milestones an issue. It only manages `needs-triage` and conflict comments.
- Issue #839 is exempt from enforcement **by number**, not by label.
- Do not run these tasks against a fork or a test repo expecting identical counts.

## File Structure

| Path | Responsibility |
|---|---|
| `scripts/triage-hygiene/reconcile.mjs` | Pure decision function. Given an issue's labels and existing comments, returns which labels to add/remove and whether to comment. No I/O. |
| `scripts/triage-hygiene/reconcile.test.mjs` | `node --test` unit tests for `reconcile()`. |
| `scripts/triage-hygiene/run.mjs` | I/O layer: fetches issues from the REST API, calls `reconcile()`, applies the result. Single-issue mode and sweep mode. |
| `.github/workflows/issue-triage-hygiene.yml` | Triggers `run.mjs` on issue events, daily schedule, and manual dispatch. |
| `scripts/triage-hygiene/snapshot.mjs` | Captures pre-migration label/milestone state and restores it. Rollback safety for Tasks 2–7. |
| `package.json` | Adds `test:triage` script. |

Migration data (the 88-issue mapping, the 33-issue roadmap manifest) lives inline in task steps as shell here-docs, not as committed files — it is one-shot migration input, not a durable artifact.

---

### Task 1: Snapshot and restore tooling

Rollback insurance before anything mutates. This must capture more than open-issue labels:
**deleting a label removes it from every associated item, including closed issues and pull
requests, and discards the label's own colour and description.** A capture run against the live
repo found 133 items carrying a doomed label, 37 of them closed — all of which a
open-issues-only snapshot would silently fail to restore. Renaming is likewise not
self-inverting, milestones created in Task 2 must be removable, and Task 7 overwrites #839's
title and body.

**Files:**
- Create: `scripts/triage-hygiene/snapshot.mjs`

**Interfaces:**
- Produces: `node scripts/triage-hygiene/snapshot.mjs capture <file>` writes `{capturedFrom, labels[], milestones[], items[], roadmapIssue}`, where `items` covers every open issue plus every item in any state carrying a doomed label. `restore <file>` first refuses to run unless `capturedFrom` matches the target repository, then recreates label definitions, restores every affected item's full label set, deletes milestones absent at capture time, restores each item's milestone, and restores #839's title and body — in that order, because associations cannot reference a label definition that does not exist yet, and a milestone cannot be restored to one that is about to be deleted. The repository comes from `GITHUB_REPOSITORY`, falling back to `GH_REPO`.

- [ ] **Step 1: Create the directory, then write the snapshot script**

Run `mkdir -p scripts/triage-hygiene` first — the file below cannot be written otherwise.

```javascript
#!/usr/bin/env node
// Capture and restore every piece of state the triage migration destroys.
//
// Deleting a label removes it from EVERY associated item, including closed
// issues and pull requests, and discards the label's own definition. Renaming
// is likewise not self-inverting. So the snapshot records label definitions,
// the full label set and milestone of every open issue plus every item
// carrying a doomed label in any state, existing milestones, and #839's title
// and body.
import { writeFileSync, readFileSync } from "node:fs";

// GITHUB_REPOSITORY wins when both are set: it is the name Actions and run.mjs
// use, so an ambient GH_REPO left over in a shell cannot redirect a restore.
const REPO = process.env.GITHUB_REPOSITORY ?? process.env.GH_REPO ?? "source-inc/gents";
const TOKEN = process.env.GITHUB_TOKEN;
// Labels this migration renames or deletes; the blast radius to capture.
const DOOMED = [
  "cluster: quality-ci",
  "cluster: context-memory",
  "cluster: network-fleet",
  "cluster: subagents-workflows",
  "cluster: inference-provider",
  "cluster: tools-control-plane",
  "cluster: clients-platform",
  "cluster: gents-cutover",
];
// Labels this migration introduces. On restore they are removed if they were
// not present at capture time, so a rollback leaves no orphaned definitions.
// Deliberately a fixed list: deleting every live label absent from the
// snapshot would destroy unrelated labels created after capture.
const INTRODUCED = ["quality-ci", "needs-triage"];
const ROADMAP_ISSUE = 839;

if (!TOKEN) {
  console.error("GITHUB_TOKEN is required");
  process.exit(1);
}

const api = async (path, init = {}) => {
  const res = await fetch(`https://api.github.com${path}`, {
    ...init,
    headers: {
      accept: "application/vnd.github+json",
      authorization: `Bearer ${TOKEN}`,
      "x-github-api-version": "2022-11-28",
      ...(init.body ? { "content-type": "application/json" } : {}),
    },
  });
  if (!res.ok) {
    const body = await res.text();
    const err = new Error(`${init.method ?? "GET"} ${path} -> ${res.status} ${body}`);
    err.status = res.status;
    throw err;
  }
  return res.status === 204 ? null : res.json();
};

const paginate = async (path) => {
  const out = [];
  for (let page = 1; ; page += 1) {
    const sep = path.includes("?") ? "&" : "?";
    const batch = await api(`${path}${sep}per_page=100&page=${page}`);
    out.push(...batch);
    if (batch.length < 100) break;
  }
  return out;
};

const capture = async (file) => {
  const labels = (await paginate(`/repos/${REPO}/labels`)).map((l) => ({
    name: l.name,
    color: l.color,
    description: l.description ?? "",
  }));

  const milestones = (await paginate(`/repos/${REPO}/milestones?state=all`)).map((m) => ({
    number: m.number,
    title: m.title,
    state: m.state,
    description: m.description ?? "",
  }));

  const items = new Map();
  const record = (it) => {
    items.set(it.number, {
      number: it.number,
      isPullRequest: Boolean(it.pull_request),
      // Informational only: restore deliberately never reopens or closes.
      state: it.state,
      labels: it.labels.map((l) => l.name).sort(),
      milestone: it.milestone ? it.milestone.title : null,
    });
  };

  // Every open issue. The migration also backfills `roadmap:` horizon labels
  // and assigns milestones across the open backlog, and none of that is
  // reachable from the doomed-label sweep below — an item can be relabelled
  // without ever having carried a doomed label.
  for (const it of await paginate(`/repos/${REPO}/issues?state=open`)) {
    if (it.pull_request) continue;
    record(it);
  }
  const openIssues = items.size;

  // Any state, so closed issues and pull requests are covered too. The Map
  // deduplicates against the open-issue sweep above by issue number.
  for (const label of DOOMED) {
    const hits = await paginate(
      `/repos/${REPO}/issues?state=all&labels=${encodeURIComponent(label)}`,
    );
    for (const it of hits) record(it);
  }

  const roadmap = await api(`/repos/${REPO}/issues/${ROADMAP_ISSUE}`);

  const snap = {
    capturedFrom: REPO,
    labels,
    milestones,
    items: [...items.values()].sort((a, b) => a.number - b.number),
    roadmapIssue: { number: ROADMAP_ISSUE, title: roadmap.title, body: roadmap.body ?? "" },
  };
  writeFileSync(file, `${JSON.stringify(snap, null, 2)}\n`);
  console.log(
    `captured ${snap.labels.length} labels, ${snap.milestones.length} milestones, ` +
      `${snap.items.length} affected items (${openIssues} open issues, the rest ` +
      `doomed-label items in any state incl. closed and PRs), #${ROADMAP_ISSUE} -> ${file}`,
  );
};

const restore = async (file) => {
  const snap = JSON.parse(readFileSync(file, "utf8"));

  // 0. This is the only destructive tool here: it deletes milestones and PUTs
  // whole label sets onto issues by number. Against the wrong repository — a
  // fork, or a mis-set GITHUB_REPOSITORY/GH_REPO — that is silent damage.
  if (snap.capturedFrom !== REPO) {
    throw new Error(
      `refusing to restore: snapshot was captured from ${snap.capturedFrom}, ` +
        `but the target repository is ${REPO}`,
    );
  }

  // 1. Recreate label definitions before any association can reference them.
  const live = new Set((await paginate(`/repos/${REPO}/labels`)).map((l) => l.name));
  for (const l of snap.labels) {
    if (live.has(l.name)) continue;
    await api(`/repos/${REPO}/labels`, {
      method: "POST",
      body: JSON.stringify({ name: l.name, color: l.color, description: l.description }),
    });
    console.log(`recreated label: ${l.name}`);
  }

  // 2. Remove labels this migration introduces that were not present at
  // capture time, so a rollback leaves no orphaned definitions.
  const captured = new Set(snap.labels.map((l) => l.name));
  for (const name of INTRODUCED) {
    if (captured.has(name)) continue;
    try {
      await api(`/repos/${REPO}/labels/${encodeURIComponent(name)}`, { method: "DELETE" });
      console.log(`deleted introduced label: ${name}`);
    } catch (err) {
      if (err.status !== 404) throw err;
    }
  }

  // 3. Restore each affected item's full label set. `state` is captured for
  // diagnosis only: restore never reopens or closes an item.
  for (const it of snap.items) {
    await api(`/repos/${REPO}/issues/${it.number}/labels`, {
      method: "PUT",
      body: JSON.stringify({ labels: it.labels }),
    });
  }
  console.log(`restored labels on ${snap.items.length} items`);

  // 4. Delete milestones that did not exist at capture time.
  const known = new Set(snap.milestones.map((m) => m.title));
  for (const m of await paginate(`/repos/${REPO}/milestones?state=all`)) {
    if (known.has(m.title)) continue;
    await api(`/repos/${REPO}/milestones/${m.number}`, { method: "DELETE" });
    console.log(`deleted milestone: ${m.title}`);
  }

  // 5. Restore each item's milestone, clearing it when none was captured.
  // Ordered after the deletion step so a captured title can never resolve to a
  // milestone that is about to be deleted.
  const byTitle = new Map(
    (await paginate(`/repos/${REPO}/milestones?state=all`)).map((m) => [m.title, m.number]),
  );
  let milestoned = 0;
  for (const it of snap.items) {
    // `null` means the item held no milestone at capture time, and PATCHing
    // null is what clears one the migration assigned.
    const title = it.milestone ?? null;
    let milestone = null;
    if (title !== null) {
      milestone = byTitle.get(title) ?? null;
      if (milestone === null) {
        console.log(`skipped #${it.number}: milestone "${title}" no longer exists`);
        continue;
      }
    }
    await api(`/repos/${REPO}/issues/${it.number}`, {
      method: "PATCH",
      body: JSON.stringify({ milestone }),
    });
    milestoned += 1;
  }
  console.log(`restored milestones on ${milestoned} items`);

  // 6. Restore the roadmap issue's title and body.
  await api(`/repos/${REPO}/issues/${snap.roadmapIssue.number}`, {
    method: "PATCH",
    body: JSON.stringify({
      title: snap.roadmapIssue.title,
      body: snap.roadmapIssue.body,
    }),
  });
  console.log(`restored #${snap.roadmapIssue.number}`);
};

const [mode, file] = process.argv.slice(2);
if (!file || (mode !== "capture" && mode !== "restore")) {
  console.error("usage: snapshot.mjs <capture|restore> <file>");
  process.exit(1);
}
await (mode === "capture" ? capture(file) : restore(file));
```

- [ ] **Step 2: Capture the baseline**

```bash
GITHUB_TOKEN=$(gh auth token) node scripts/triage-hygiene/snapshot.mjs capture /tmp/gents-triage-baseline.json
```

Expected: `captured 33 labels, 0 milestones, N affected items (M open issues, the rest
doomed-label items in any state incl. closed and PRs), #839 -> ...`

- [ ] **Step 3: Verify the snapshot covers the full blast radius**

```bash
jq '{labels:(.labels|length), milestones:(.milestones|length), items:(.items|length),
     closed:([.items[]|select(.state=="closed")]|length),
     open:([.items[]|select(.state=="open")]|length),
     roadmapBody:(.roadmapIssue.body|length)}' /tmp/gents-triage-baseline.json
cp /tmp/gents-triage-baseline.json docs/superpowers/plans/triage-baseline-2026-08-12.json
```

Expected: 33 labels, 0 milestones, at least 133 items of which 37 closed, and a non-zero
`roadmapBody`. **If `milestones` is not 0, milestones already exist and Task 2 would collide —
stop.** If the doomed-label item count is far from 133, the issue set moved since planning and
the manifests in Tasks 4–6 need re-derivation.

> **Known limit of the committed baseline.** `docs/superpowers/plans/triage-baseline-2026-08-12.json`
> was captured before `capture()` was widened to sweep every open issue, so its `items` list holds
> only the 133 doomed-label items. It cannot be re-captured: the tracker is already migrated, so a
> fresh capture would record post-migration state and invert nothing.
>
> Restoring **that specific file** therefore reverts cluster-label associations, label definitions,
> milestones, and #839 — but leaves in place the 29 `roadmap:` horizon labels that Task 6 backfilled
> onto issues absent from `items`. Those must be removed by hand after a rollback:
>
> 52, 884, 897, 980, 1035, 1036, 1041, 1044, 1045, 1047, 1048, 1049, 1054, 1063, 1064, 1071, 1072,
> 1073, 1074, 1075, 1077, 1078, 1079, 1082, 1083, 1084, 1085, 1086, 1090
>
> Any snapshot captured with the current script does not have this gap.

- [ ] **Step 4: Verify restore is a genuine inverse on one item**

Prove the rollback path works before relying on it, using a label the migration does not touch.

```bash
n=$(jq -r '.items[0].number' /tmp/gents-triage-baseline.json)
jq -r --argjson n "$n" '.items[]|select(.number==$n)|.labels|join(",")' /tmp/gents-triage-baseline.json
gh issue edit "$n" --add-label "documentation"
GITHUB_TOKEN=$(gh auth token) node scripts/triage-hygiene/snapshot.mjs restore /tmp/gents-triage-baseline.json
gh issue view "$n" --json labels -q '[.labels[].name]|sort|join(",")'
```

Expected: the final label set matches the snapshot line exactly, with `documentation` gone.
A restore that leaves the stray label means `PUT /labels` is not replacing the set — stop and
fix before any destructive task.

- [ ] **Step 5: Commit**

```bash
git add scripts/triage-hygiene/snapshot.mjs docs/superpowers/plans/triage-baseline-2026-08-12.json
git commit -m "chore(triage): capture full pre-migration label, milestone, and roadmap state"
```

---

### Task 2: Create the eight milestones

**Files:** none (GitHub state only)

**Interfaces:**
- Produces: eight open milestones whose exact titles Task 4 references.

- [ ] **Step 1: Create all eight with descriptions**

```bash
set -euo pipefail
create() { gh api "repos/source-inc/gents/milestones" -f title="$1" -f description="$2" -f state=open >/dev/null && echo "created: $1"; }

create "Authority and provenance hardening" \
"Parent: #1063. Done when intent and execution authority are separate documents, every provider send and tool fact binds an exact _docID + composite CID, and no writer can forge or silently rewrite a fact it does not own.
Order: #1071 -> #1073 -> #1075 -> #1079 -> #1072/#1074 -> #1077 -> #1064 -> #1044/#1045/#1048 -> #1078"

create "Fleet convergence and P2P durability" \
"Done when a fresh 19-node control mesh converges without runaway pending DAGs, and a failed push of a stable document retries instead of leaving it permanently missing on the peer.
Order: #630/#696/#798 as one diagnosis -> #1036 -> #977 -> #1049 -> #987 -> remainder"

create "Multi-agent coordination" \
"Parent: #832. Architecture: #378, #835. Done when a parent has one descendant graph across behaviors, await modes, deployments, and workflow roles; children have enforceable budgets; and a composed run is losslessly inspectable and continuation-safe.
Order: #836 -> #835 -> #838 -> #834 -> #734
External prerequisites owned by Long-context correctness: #716, #722."

create "Long-context correctness" \
"Done when token estimation is model-aware and centralized, compaction budgets are coherent and validated with a defined failure path, and trimmed evidence is recoverable.
Order: #718 -> #719 -> #717 -> #716 -> #722, then #1012/#1025/#720/#1009"

create "Durable trace, attribution and trust" \
"Done when every operational output carries generation and configuration attribution, the run timeline projects the complete durable request record, and export redaction is bound to ACP rather than caller choice.
Order: #842 -> #845 -> #846 -> #841 -> #843 -> #844. Decompose #847 before promotion and reconcile against #461 and #539 rather than building parallel authorization paths."

create "Provider fidelity and rig removal" \
"Done when rig-core is gone, the provider wire contract has a support matrix and replay fixtures, and token usage is complete and attributable by inference call, request, and session.
Order: #991 -> #509/#726 -> #438 -> #439"

create "iOS hardening" \
"Done when iOS has a distribution path beyond development export, identity keys and the node directory are protected, replication and streaming survive app suspend, and CI covers the simulator build and E2E lane.
Order: #892 -> #896/#894 -> #893 -> #895 -> #891 -> #890"

create "Gents cutover" \
"Done when #811 closes."
```

- [ ] **Step 2: Verify all eight exist and are empty**

```bash
gh api repos/source-inc/gents/milestones --paginate \
  -q '.[] | "\(.title)\topen=\(.open_issues)"'
gh api repos/source-inc/gents/milestones --paginate -q 'length'
```

Expected: 8 milestones, every `open=0`. If a title has a typo, fix it now with `gh api --method PATCH repos/source-inc/gents/milestones/<n> -f title=...` — Task 4 matches on exact title.

---

### Task 3: Add the `quality-ci` and `needs-triage` labels

Renaming before any deletion is what preserves the 14 existing `cluster: quality-ci` associations.

**Files:** none (GitHub state only)

- [ ] **Step 1: Rename the one cluster label we keep**

```bash
gh label edit "cluster: quality-ci" --name "quality-ci" \
  --description "Flaky tests, CI infrastructure, and cross-suite reliability"
gh label create "needs-triage" --color "d876e3" \
  --description "Missing exactly one roadmap: label" --force
```

- [ ] **Step 2: Verify the rename preserved associations**

```bash
gh issue list --state open --label "quality-ci" --json number -q length   # expect 14
gh label list --limit 200 | grep -c "^cluster: "                          # expect 7
```

Expected: 14 issues still carry the label under its new name, and 7 `cluster:` labels remain (quality-ci is no longer among them). **If the count is 0, the rename silently created a new label — stop and restore from the baseline snapshot.**

- [ ] **Step 3: Backfill `quality-ci` onto the eight unlabelled members**

```bash
for n in 52 884 1035 1041 1082 1083 1084 1085; do
  gh issue edit "$n" --add-label "quality-ci" && echo "labelled #$n"
done
gh issue list --state open --label "quality-ci" --json number -q length   # expect 22
```

Expected: 22 — the 21 unmilestoned members plus #890, which keeps the label inside the iOS milestone.

---

### Task 4: Assign 88 issues to milestones

**Files:** none (GitHub state only)

**Interfaces:**
- Consumes: the eight milestone titles created in Task 2.

- [ ] **Step 1: Apply the mapping**

```bash
set -euo pipefail
assign() {
  local ms="$1"; shift
  for n in "$@"; do
    gh issue edit "$n" --milestone "$ms" >/dev/null && echo "#$n -> $ms"
  done
}

assign "Authority and provenance hardening" 1063 1064 1071 1072 1073 1074 1075 1077 1078 1079 1044 1045 1048
assign "Fleet convergence and P2P durability" 630 696 798 977 987 938 678 679 660 606 1049 1036 366 673
assign "Multi-agent coordination" 378 832 834 835 836 838 734 937 1096 564 868 728
assign "Long-context correctness" 716 717 718 719 720 722 723 725 1009 1012 1025 391 523 652 1054
assign "Durable trace, attribution and trust" 841 842 843 844 845 846 847 461 539 749 882 621
assign "Provider fidelity and rig removal" 438 439 498 509 514 540 708 726 737 741 748 991 897 1047
assign "iOS hardening" 890 891 892 893 894 895 896
assign "Gents cutover" 811
```

- [ ] **Step 2: Verify per-milestone counts**

```bash
gh api repos/source-inc/gents/milestones --paginate \
  -q '.[] | "\(.open_issues)\t\(.title)"' | sort -rn
```

Expected exactly:

```
15	Long-context correctness
14	Fleet convergence and P2P durability
14	Provider fidelity and rig removal
13	Authority and provenance hardening
12	Durable trace, attribution and trust
12	Multi-agent coordination
7	iOS hardening
1	Gents cutover
```

Total 88. Any other number means an issue number in Step 1 was wrong or already closed.

- [ ] **Step 3: Verify the unmilestoned remainder**

```bash
gh issue list --state open --limit 500 --search "no:milestone" --json number -q length   # expect 39
gh issue list --state open --limit 500 --search "no:milestone -label:quality-ci" --json number,title -q '.[]|"#\(.number) \(.title)"'
```

Expected: 39 unmilestoned; the second command lists 18 issues — 527, 543, 580, 608, 647, 700, 705, 732, 739, 742, 833, 839, 849, 899, 980, 986, 1086, 1090.

---

### Task 5: Backfill the 33 missing `roadmap:` labels

**Files:** none (GitHub state only)

- [ ] **Step 1: Apply the manifest**

```bash
set -euo pipefail
horizon() {
  local label="$1"; shift
  for n in "$@"; do
    gh issue edit "$n" --add-label "$label" >/dev/null && echo "#$n -> $label"
  done
}

horizon "roadmap: now"   1071 1093
horizon "roadmap: next"  1063 1064 1072 1073 1074 1075 1077 1079 1044 1045 1048 1036 1049 884
horizon "roadmap: later" 52 330 975 1035 1041 1082 1083 1084 1085 1086 1090 897 1047 1054 1096 980 1078
```

- [ ] **Step 2: Verify every open issue now has exactly one horizon**

```bash
gh issue list --state open --limit 500 --json number,labels \
  -q '[.[] | {n: .number, c: ([.labels[].name] | map(select(startswith("roadmap:"))) | length)}]
      | map(select(.c != 1)) | .[] | "#\(.n) has \(.c) roadmap labels"'
```

Expected: **no output.** Any line is a violation to fix before continuing.

- [ ] **Step 3: Verify horizon distribution**

```bash
for h in now next later parked; do
  printf "%s\t%s\n" "$h" "$(gh issue list --state open --limit 500 --label "roadmap: $h" --json number -q length)"
done
```

Expected: `now` 7, and `now + next + later + parked = 127`. If `now` exceeds 10, the WIP limit in the rewritten #839 is already violated — stop and re-triage rather than proceeding.

---

### Task 6: Delete the seven cluster labels

Destructive and irreversible except via the Task 1 snapshot. Do not start until Tasks 4 and 5 verify clean.

**Files:** none (GitHub state only)

- [ ] **Step 1: Confirm the milestones fully cover what the clusters recorded**

```bash
gh issue list --state open --limit 500 --json number,labels,milestone \
  -q '[.[] | select([.labels[].name] | any(startswith("cluster:")))
       | select(.milestone == null) | .number]'
```

Expected: a JSON array containing only issues that are *intentionally* unmilestoned — the quality-ci and platform sets. Review it before deleting. Anything surprising here means a mapping gap.

- [ ] **Step 2: Delete**

```bash
for l in "cluster: context-memory" "cluster: network-fleet" "cluster: subagents-workflows" \
         "cluster: inference-provider" "cluster: tools-control-plane" \
         "cluster: clients-platform" "cluster: gents-cutover"; do
  gh label delete "$l" --yes && echo "deleted: $l"
done
```

- [ ] **Step 3: Verify none remain**

```bash
gh label list --limit 200 | grep "^cluster: " || echo "no cluster labels remain"
gh issue list --state open --label "quality-ci" --json number -q length   # expect 22, unaffected
```

Expected: `no cluster labels remain`, and `quality-ci` still on 22.

---

### Task 7: Rewrite #839

**Files:** none (GitHub state only)

- [ ] **Step 1: Archive the current body**

```bash
gh issue view 839 --json body -q .body > /tmp/839-original.md
wc -l /tmp/839-original.md
```

- [ ] **Step 2: Write the new body**

Write `/tmp/839-new.md` containing, in order: horizon semantics for the four `roadmap:` labels (copied verbatim from the original — that section is unchanged); the rewritten work-in-progress rules below; the retained "Why there is no GitHub Project yet" section with its trigger conditions updated per the spec; and links to the four horizon queries plus the milestone list.

The work-in-progress rules replace the old cluster-based ones entirely:

```markdown
## Work-in-progress rules

- At most one implementation issue per **milestone** is active.
- At most **ten** issues sit in `roadmap: now` repository-wide.
- Design and proof work may overlap implementation only when it feeds the next executable slice.
- Moving an issue into `now` should move something else out unless it is a production invariant
  or release blocker.

The ten-issue repository limit replaces the previous three-issue implementation limit because
`now` holds non-implementation work too — CI infrastructure, bugs, and this issue — which the
old rule silently did not count. Ten also matches the threshold for creating a GitHub Project
below, so the two rules cannot disagree.
```

Delete from the body: the "Current program: hard cutover to Gents" section (complete except #811), and the five prose program sections (A, B, C, D and the horizontal quality lane). That content now lives in milestone descriptions.

- [ ] **Step 3: Apply and retitle**

```bash
gh issue edit 839 --body-file /tmp/839-new.md \
  --title "Operating rules: horizons, work-in-progress limits, and the Project revisit gate"
gh issue view 839 --json title,body -q '.title'
```

- [ ] **Step 4: Verify no program contents were duplicated**

```bash
grep -cE "^[0-9]+\. #[0-9]+" /tmp/839-new.md || echo "no ordered issue lists remain"
```

Expected: `no ordered issue lists remain`. Ordered critical paths belong in milestone descriptions only; if this matches, the rewrite reintroduced the duplication the spec removes.

---

### Task 8: The `reconcile()` decision function

Pure logic, no I/O, so the rules are testable without touching the API. This is the only real code in the plan and it is written test-first.

**Files:**
- Create: `scripts/triage-hygiene/reconcile.mjs`
- Test: `scripts/triage-hygiene/reconcile.test.mjs`
- Modify: `package.json`

**Interfaces:**
- Produces: `reconcile({ number, labels, comments }) -> { add: string[], remove: string[], comment: string|null }` and `requiresCommentContext({ number, labels }) -> boolean`, which tells a caller whether `reconcile()` could possibly return a comment so it knows whether fetching comment context is worthwhile; plus the constants `NEEDS_TRIAGE`, `ROADMAP_PREFIX`, `HORIZONS`, `EXEMPT_ISSUES`, and `conflictMarker(labels)`. Task 9's runner consumes all of these.

- [ ] **Step 1: Write the failing tests**

```javascript
import { test } from "node:test";
import assert from "node:assert/strict";
import { reconcile, conflictMarker, requiresCommentContext, NEEDS_TRIAGE } from "./reconcile.mjs";

const base = { number: 1, labels: [], comments: [] };

test("exactly one horizon and no flag is already correct", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", "bug"] });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("exactly one horizon clears an existing flag", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", NEEDS_TRIAGE] });
  assert.deepEqual(r, { add: [], remove: [NEEDS_TRIAGE], comment: null });
});

test("every defined horizon is accepted", () => {
  for (const h of ["roadmap: now", "roadmap: next", "roadmap: later", "roadmap: parked"]) {
    assert.deepEqual(
      reconcile({ ...base, labels: [h] }),
      { add: [], remove: [], comment: null },
      `${h} should be valid`,
    );
  }
});

test("no horizon adds the flag without commenting", () => {
  const r = reconcile({ ...base, labels: ["bug"] });
  assert.deepEqual(r, { add: [NEEDS_TRIAGE], remove: [], comment: null });
});

test("no horizon with the flag already set writes nothing", () => {
  const r = reconcile({ ...base, labels: ["bug", NEEDS_TRIAGE] });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("two horizons flag and comment naming both labels", () => {
  const r = reconcile({ ...base, labels: ["roadmap: now", "roadmap: later"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
  assert.equal(r.remove.length, 0);
  assert.match(r.comment, /roadmap: later/);
  assert.match(r.comment, /roadmap: now/);
  assert.ok(r.comment.includes(conflictMarker(["roadmap: now", "roadmap: later"])));
});

test("an unknown roadmap label does not satisfy the rule", () => {
  const r = reconcile({ ...base, labels: ["roadmap: urgent"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
  assert.match(r.comment, /not a defined horizon/);
  assert.match(r.comment, /roadmap: urgent/);
});

test("one valid horizon plus one unknown is a conflict", () => {
  const r = reconcile({ ...base, labels: ["roadmap: next", "roadmap: urgent"] });
  assert.deepEqual(r.add, [NEEDS_TRIAGE]);
  assert.match(r.comment, /roadmap: urgent/);
  assert.ok(r.comment.includes(conflictMarker(["roadmap: next", "roadmap: urgent"])));
});

test("an existing comment for the same conflict is not repeated", () => {
  const marker = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", "roadmap: later", NEEDS_TRIAGE],
    comments: [`stale text ${marker} more text`],
  });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("a different conflict set comments again", () => {
  const old = conflictMarker(["roadmap: now", "roadmap: later"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: now", "roadmap: parked", NEEDS_TRIAGE],
    comments: [`older ${old}`],
  });
  assert.notEqual(r.comment, null);
});

test("the marker is independent of label order", () => {
  assert.equal(
    conflictMarker(["roadmap: now", "roadmap: later"]),
    conflictMarker(["roadmap: later", "roadmap: now"]),
  );
});

test("the marker is injective across label sets that share a separator", () => {
  assert.notEqual(
    conflictMarker(["a|b"]),
    conflictMarker(["a", "b"]),
  );
});

test("issue 839 is exempt by number even with no horizon", () => {
  const r = reconcile({ ...base, number: 839, labels: ["meta"] });
  assert.deepEqual(r, { add: [], remove: [], comment: null });
});

test("a future meta issue is not exempt", () => {
  const r = reconcile({ ...base, number: 2000, labels: ["meta"] });
  assert.deepEqual(r, { add: [NEEDS_TRIAGE], remove: [], comment: null });
});

test("requiresCommentContext: exactly one valid horizon does not require comments", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["roadmap: next", "bug"] }), false);
});

test("requiresCommentContext: no roadmap labels at all does not require comments", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["bug"] }), false);
});

test("requiresCommentContext: exactly one invalid roadmap label requires comments", () => {
  assert.equal(requiresCommentContext({ number: 1, labels: ["roadmap: soon", "bug"] }), true);
});

test("requiresCommentContext: two valid horizons requires comments", () => {
  assert.equal(
    requiresCommentContext({ number: 1, labels: ["roadmap: now", "roadmap: later"] }),
    true,
  );
});

test("requiresCommentContext: exempt issue 839 with two horizons does not require comments", () => {
  assert.equal(
    requiresCommentContext({ number: 839, labels: ["roadmap: now", "roadmap: later"] }),
    false,
  );
});

// requiresCommentContext() is run.mjs's gate on fetching comments at all, so if
// it ever says "no comments needed" for a label set that reconcile() would
// comment on, the dedup check runs against an empty comment list and the bot
// reposts the same conflict comment on every sweep. That exact pair drifted
// apart once already. Five hand-picked examples cannot fence an equivalence;
// enumerate the space instead.
const ALPHABET = [
  "roadmap: now",
  "roadmap: next",
  "roadmap: later",
  "roadmap: parked",
  "roadmap: soon", // unknown horizon: `roadmap:`-prefixed but not defined
  "bug", // not roadmap-prefixed at all
  NEEDS_TRIAGE,
];

const labelSetsUpToSize = (alphabet, maxSize) => {
  const sets = [[]];
  const extend = (prefix, start) => {
    if (prefix.length === maxSize) return;
    for (let i = start; i < alphabet.length; i += 1) {
      const next = [...prefix, alphabet[i]];
      sets.push(next);
      extend(next, i + 1);
    }
  };
  extend([], 0);
  return sets;
};

test("property: not requiring comment context implies reconcile never comments", () => {
  const sets = labelSetsUpToSize(ALPHABET, 3);
  assert.equal(sets.length, 64, "expected every subset of size 0..3");
  let checked = 0;
  for (const number of [1, 839]) {
    for (const labels of sets) {
      if (requiresCommentContext({ number, labels })) continue;
      const r = reconcile({ number, labels, comments: [] });
      assert.equal(
        r.comment,
        null,
        `#${number} with labels ${JSON.stringify(labels)}: requiresCommentContext() said ` +
          `no comment context was needed, but reconcile() produced a comment. run.mjs would ` +
          `post it without ever having read the existing comments, so it would repost forever.`,
      );
      checked += 1;
    }
  }
  assert.ok(checked > 0, "the property must actually exercise some label sets");
});

test("regression: single invalid roadmap label dedupes against a prior conflict comment", () => {
  const marker = conflictMarker(["roadmap: soon"]);
  const r = reconcile({
    ...base,
    labels: ["roadmap: soon", NEEDS_TRIAGE],
    comments: [`stale text ${marker} more text`],
  });
  assert.equal(r.comment, null);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
node --test scripts/triage-hygiene/*.test.mjs
```

Expected: FAIL — `Cannot find module './reconcile.mjs'`.

- [ ] **Step 3: Implement**

```javascript
// Pure triage-hygiene decision logic. No I/O — see run.mjs for the API layer.
export const NEEDS_TRIAGE = "needs-triage";
export const ROADMAP_PREFIX = "roadmap:";
// The taxonomy defines exactly four horizons. Anything else under the
// `roadmap:` prefix is a typo, not a horizon, and must not satisfy the rule.
export const HORIZONS = new Set([
  "roadmap: now",
  "roadmap: next",
  "roadmap: later",
  "roadmap: parked",
]);
// Exempt by issue NUMBER, not by label: exempting the `meta` label would
// silently exempt every future meta issue.
export const EXEMPT_ISSUES = new Set([839]);

const MARKER_OPEN = "<!-- triage-hygiene:conflict:";

// Use JSON encoding to ensure the marker is unambiguous: label names may
// contain any character including separator characters, so encoding must be injective.
export function conflictMarker(labels) {
  return `${MARKER_OPEN}${JSON.stringify([...labels].sort())} -->`;
}

// Tells a caller whether reconcile() could possibly return a non-null comment
// for this issue, so it knows whether fetching comment context is worthwhile.
// Mirrors reconcile()'s clean-condition exactly; kept here so run.mjs never
// re-derives the rule and risks drifting from it.
export function requiresCommentContext({ number, labels }) {
  if (EXEMPT_ISSUES.has(number)) return false;
  const roadmap = labels.filter((l) => l.startsWith(ROADMAP_PREFIX));
  if (roadmap.length === 0) return false;
  if (roadmap.length === 1 && HORIZONS.has(roadmap[0])) return false;
  return true;
}

export function reconcile({ number, labels, comments = [] }) {
  const noop = { add: [], remove: [], comment: null };
  if (EXEMPT_ISSUES.has(number)) return noop;

  const roadmap = labels.filter((l) => l.startsWith(ROADMAP_PREFIX)).sort();
  const unknown = roadmap.filter((l) => !HORIZONS.has(l));
  const flagged = labels.includes(NEEDS_TRIAGE);

  // Clean iff exactly one roadmap label AND it is a defined horizon.
  if (roadmap.length === 1 && unknown.length === 0) {
    return flagged ? { add: [], remove: [NEEDS_TRIAGE], comment: null } : noop;
  }

  // Only mutate on a state change; redundant writes create timeline noise and
  // can ping-pong via labeled/unlabeled events from non-GITHUB_TOKEN actors.
  const add = flagged ? [] : [NEEDS_TRIAGE];

  // Nothing to explain when the issue simply has no horizon yet.
  if (roadmap.length === 0) return { add, remove: [], comment: null };

  const marker = conflictMarker(roadmap);
  if (comments.some((body) => body.includes(marker))) {
    return { add, remove: [], comment: null };
  }

  const detail =
    unknown.length > 0
      ? `${unknown.map((l) => `\`${l}\``).join(", ")} ${unknown.length === 1 ? "is not a" : "are not"} defined horizon${unknown.length === 1 ? "" : "s"}. Valid horizons: ${[...HORIZONS].map((l) => `\`${l}\``).join(", ")}.`
      : `It carries ${roadmap.length} horizons: ${roadmap.map((l) => `\`${l}\``).join(", ")}.`;

  return {
    add,
    remove: [],
    comment:
      `This issue does not carry exactly one valid \`roadmap:\` horizon. ${detail} ` +
      `See #839. Fixing the labels clears \`${NEEDS_TRIAGE}\`.\n\n${marker}`,
  };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
node --test scripts/triage-hygiene/*.test.mjs
```

Expected: PASS, 21/21 — 20 example tests plus the property test that enumerates every label set of
size 0–3 over a seven-label alphabet and asserts `requiresCommentContext()` never says "no comment
context needed" for a set `reconcile()` would comment on.

- [ ] **Step 5: Wire up the npm script and commit**

Add to `package.json` `scripts`: `"test:triage": "node --test scripts/triage-hygiene/*.test.mjs"`.

```bash
npm run test:triage
git add scripts/triage-hygiene/reconcile.mjs scripts/triage-hygiene/reconcile.test.mjs package.json
git commit -m "feat(triage): add pure roadmap-label reconcile logic with tests"
```

---

### Task 9: The runner

**Files:**
- Create: `scripts/triage-hygiene/run.mjs`

**Interfaces:**
- Consumes: `reconcile`, `requiresCommentContext`, `NEEDS_TRIAGE` from `./reconcile.mjs`. `requiresCommentContext` gates the comment fetch, so the sweep pays for comment pages only on issues whose plan could include a comment.
- Produces: `node scripts/triage-hygiene/run.mjs` — sweeps all open issues, or reconciles one when `ISSUE_NUMBER` is set. Setting `DRY_RUN` to anything other than `0`, `false`, or the empty string prints without mutating.

- [ ] **Step 1: Implement**

```javascript
#!/usr/bin/env node
import { reconcile, requiresCommentContext, NEEDS_TRIAGE } from "./reconcile.mjs";

const REPO = process.env.GITHUB_REPOSITORY ?? "source-inc/gents";
const TOKEN = process.env.GITHUB_TOKEN;
// Deliberately permissive: this bot holds `issues: write` on the production
// tracker, so a typo'd truthy value (`DRY_RUN=true`, `DRY_RUN=yes`) must never
// silently enable live writes. Only the explicit negatives disable dry-run;
// an unset variable means disabled.
const DRY_RUN_RAW = (process.env.DRY_RUN ?? "").trim().toLowerCase();
const DRY_RUN = DRY_RUN_RAW !== "" && DRY_RUN_RAW !== "0" && DRY_RUN_RAW !== "false";
if (!TOKEN) {
  console.error("GITHUB_TOKEN is required");
  process.exit(1);
}

const api = async (path, init = {}) => {
  const res = await fetch(`https://api.github.com${path}`, {
    ...init,
    headers: {
      accept: "application/vnd.github+json",
      authorization: `Bearer ${TOKEN}`,
      "x-github-api-version": "2022-11-28",
      ...(init.body ? { "content-type": "application/json" } : {}),
    },
  });
  if (!res.ok) {
    const err = new Error(`${init.method ?? "GET"} ${path} -> ${res.status} ${await res.text()}`);
    err.status = res.status;
    throw err;
  }
  return res.status === 204 ? null : res.json();
};

const listOpenIssues = async () => {
  const out = [];
  for (let page = 1; ; page += 1) {
    const batch = await api(`/repos/${REPO}/issues?state=open&per_page=100&page=${page}`);
    out.push(...batch.filter((i) => !i.pull_request));
    if (batch.length < 100) break;
  }
  return out;
};

// Must be exhaustive, not just the first page: GitHub returns issue comments
// oldest-first, so on an issue with more than 100 comments a prior conflict
// marker sits on a later page. Missing it defeats the dedup check and reposts
// the same conflict comment on every sweep, forever.
const listComments = async (number) => {
  const out = [];
  for (let page = 1; ; page += 1) {
    const batch = await api(`/repos/${REPO}/issues/${number}/comments?per_page=100&page=${page}`);
    out.push(...batch.map((c) => c.body ?? ""));
    if (batch.length < 100) break;
  }
  return out;
};

const applyTo = async (issue) => {
  const labels = issue.labels.map((l) => (typeof l === "string" ? l : l.name));
  // Comments are only needed to dedupe conflict notices, so fetch them lazily.
  const comments = requiresCommentContext({ number: issue.number, labels })
    ? await listComments(issue.number)
    : [];

  const plan = reconcile({ number: issue.number, labels, comments });
  if (!plan.add.length && !plan.remove.length && !plan.comment) return false;

  console.log(`#${issue.number}: +[${plan.add}] -[${plan.remove}] comment=${Boolean(plan.comment)}`);
  if (DRY_RUN) return true;

  if (plan.add.length) {
    await api(`/repos/${REPO}/issues/${issue.number}/labels`, {
      method: "POST",
      body: JSON.stringify({ labels: plan.add }),
    });
  }
  for (const name of plan.remove) {
    await api(`/repos/${REPO}/issues/${issue.number}/labels/${encodeURIComponent(name)}`, {
      method: "DELETE",
    }).catch((err) => {
      // A 404 means the label is already gone, which is the desired end state.
      // Anything else — permissions, rate limits, 5xx — must not be reported
      // as a successful reconcile.
      if (err.status !== 404) throw err;
    });
  }
  if (plan.comment) {
    await api(`/repos/${REPO}/issues/${issue.number}/comments`, {
      method: "POST",
      body: JSON.stringify({ body: plan.comment }),
    });
  }
  return true;
};

const single = process.env.ISSUE_NUMBER;
const targets = single
  ? [await api(`/repos/${REPO}/issues/${single}`)]
  : await listOpenIssues();

let changed = 0;
for (const issue of targets) {
  if (issue.state !== "open" || issue.pull_request) continue;
  if (await applyTo(issue)) changed += 1;
}
// Distinguish intent from effect: nothing was written in dry-run mode.
console.log(
  `${NEEDS_TRIAGE} reconcile complete: ${targets.length} examined, ` +
    `${changed} ${DRY_RUN ? "would change" : "changed"}`,
);
```

- [ ] **Step 2: Dry-run the sweep against live state**

```bash
DRY_RUN=1 GITHUB_TOKEN=$(gh auth token) node scripts/triage-hygiene/run.mjs
```

Expected: `127 examined, 0 would change` — dry runs report intent, not effect. Tasks 5 and 6 left the backlog clean, so a correct implementation finds nothing to do. **Any non-zero change count means either the backfill is incomplete or `reconcile()` is wrong — diagnose before landing the workflow.**

- [ ] **Step 3: Prove it detects a real violation**

```bash
gh issue edit 899 --add-label "roadmap: now"     # 899 already has roadmap: later
DRY_RUN=1 GITHUB_TOKEN=$(gh auth token) ISSUE_NUMBER=899 node scripts/triage-hygiene/run.mjs
gh issue edit 899 --remove-label "roadmap: now"  # restore
```

Expected: the dry run reports `#899: +[needs-triage] -[] comment=true`, then the restore returns it to one horizon.

- [ ] **Step 4: Commit**

```bash
git add scripts/triage-hygiene/run.mjs
git commit -m "feat(triage): add triage-hygiene runner with sweep and single-issue modes"
```

---

### Task 10: The workflow

**Files:**
- Create: `.github/workflows/issue-triage-hygiene.yml`

**Interfaces:**
- Consumes: `scripts/triage-hygiene/run.mjs`.

- [ ] **Step 1: Write the workflow**

```yaml
name: issue-triage-hygiene

on:
  issues:
    types: [opened, reopened, labeled, unlabeled]
  schedule:
    - cron: "17 6 * * *"
  workflow_dispatch:

permissions:
  # contents: read is required for actions/checkout; declaring `permissions`
  # at all drops every scope not listed here.
  contents: read
  issues: write

concurrency:
  # One global group, not a per-issue group with a unique fallback: a sweep and
  # a per-issue run touching the same issue must not interleave, and two sweeps
  # must not overlap. `cancel-in-progress: false` lets the running job finish
  # rather than being killed mid-write; GitHub still keeps only the newest
  # PENDING run in the group, so an intermediate run during a bulk relabel can
  # be dropped. That is safe: the next sweep reconciles the whole backlog.
  group: triage-hygiene
  cancel-in-progress: false

jobs:
  reconcile:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: actions/setup-node@v7
        with:
          node-version: "22"
      - name: Reconcile roadmap labels
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          GITHUB_REPOSITORY: ${{ github.repository }}
          # Absent on schedule/dispatch, which makes run.mjs sweep every open issue.
          ISSUE_NUMBER: ${{ github.event.issue.number }}
        run: node scripts/triage-hygiene/run.mjs
```

Three details are load-bearing:

- **`contents: read`.** Declaring a `permissions` block drops every scope not listed, so
  `issues: write` alone leaves `actions/checkout` unable to read the repository.
- **A single global concurrency group.** Falling back to `github.run_id` would give every
  scheduled sweep a unique group, so sweeps could overlap each other and per-issue runs —
  permitting duplicate conflict comments and decisions made from stale label state.
- **`actions/checkout@v7`, `actions/setup-node@v7`, Node 22**, matching the 15 and 8 existing
  usages across `.github/workflows/`. Do not introduce a second version line.

`opened` and `reopened` run the same full reconcile as every other trigger rather than blindly adding the flag. This is required for correctness, not tidiness: label mutations made with the default `GITHUB_TOKEN` do not trigger further workflow runs, so a blind add would depend on a `labeled` event that never fires and would leave a correctly-labelled reopened issue flagged until the next daily sweep.

- [ ] **Step 2: Commit and push the branch**

```bash
git add .github/workflows/issue-triage-hygiene.yml
git commit -m "ci(triage): enforce exactly one roadmap label per open issue"
git push -u origin HEAD
```

- [ ] **Step 3: Verify with a manual dispatch after merge**

```bash
gh workflow run issue-triage-hygiene
sleep 10
run_id=$(gh run list --workflow=issue-triage-hygiene --limit 1 --json databaseId -q '.[0].databaseId')
# --job expects a JOB id, not a run id; watch the run, then read its log.
gh run watch "$run_id" --exit-status
gh run view "$run_id" --log | tail -20
```

Expected: the run succeeds and logs `127 examined, 0 changed`.

- [ ] **Step 4: Verify the live path end to end**

```bash
# gh issue create prints the issue URL; it has no --json flag.
url=$(gh issue create --title "triage hygiene smoke test" --body "Delete me.")
n=${url##*/}
sleep 45
gh issue view "$n" --json labels -q '[.labels[].name]'   # expect ["needs-triage"]
gh issue edit "$n" --add-label "roadmap: later"
sleep 45
gh issue view "$n" --json labels -q '[.labels[].name]'   # expect ["roadmap: later"]
gh issue close "$n" --reason "not planned"
```

Expected: the flag appears on creation and clears once a single horizon is applied. This exercises exactly the cascade the naive design got wrong.

---

### Task 11: Verify against the spec's success criteria

**Files:** none

- [ ] **Step 1: Verify the issue-to-milestone mapping exactly, not just its totals**

Aggregate counts pass even when two issues are swapped between milestones. Compare the live
mapping to the manifest itself.

```bash
LC_ALL=C sort > /tmp/expected-mapping.txt <<'EOF'
Authority and provenance hardening|1044 1045 1048 1063 1064 1071 1072 1073 1074 1075 1077 1078 1079
Durable trace, attribution and trust|461 539 621 749 841 842 843 844 845 846 847 882
Fleet convergence and P2P durability|366 606 630 660 673 678 679 696 798 938 977 987 1036 1049
Gents cutover|811
Long-context correctness|391 523 652 716 717 718 719 720 722 723 725 1009 1012 1025 1054
Multi-agent coordination|378 564 728 734 832 834 835 836 838 868 937 1096
Provider fidelity and rig removal|438 439 498 509 514 540 708 726 737 741 748 897 991 1047
iOS hardening|890 891 892 893 894 895 896
EOF

gh issue list --state open --limit 500 --json number,milestone \
  -q '[.[]|select(.milestone!=null)|{t:.milestone.title,n:.number}]
      | group_by(.t) | map("\(.[0].t)|\([.[].n]|sort|join(" "))") | .[]' \
  | LC_ALL=C sort > /tmp/actual-mapping.txt

diff /tmp/expected-mapping.txt /tmp/actual-mapping.txt && echo "MAPPING EXACT"
```

Expected: `MAPPING EXACT`. Any diff line names the milestone and the issue numbers that drifted.

- [ ] **Step 2: Run the remaining criteria**

```bash
set -u
echo "1. milestones (expect 8, each with a description):"
gh api repos/source-inc/gents/milestones --paginate -q '.[] | "\(.open_issues)\t\(.title)\tdesc=\(.description|length>0)"'

echo "2. milestoned (expect 88) / unmilestoned (expect 39):"
gh issue list --state open --limit 500 --json milestone -q '[.[]|select(.milestone!=null)]|length'
gh issue list --state open --limit 500 --json milestone -q '[.[]|select(.milestone==null)]|length'

echo "3. horizon violations (expect only the #839 line), now (expect 7):"
gh issue list --state open --limit 500 --json number,labels \
  -q '[.[] | {n:.number, c:([.labels[].name]|map(select(startswith("roadmap:")))|length)}]
      | map(select(.c != 1)) | .[] | "#\(.n) has \(.c)"'
gh issue list --state open --limit 500 --label "roadmap: now" --json number -q length

echo "4. cluster labels (expect none), quality-ci (expect 22):"
gh label list --limit 200 | grep "^cluster: " || echo "  none"
gh issue list --state open --label "quality-ci" --json number -q length

echo "5. backlog (expect 18):"
gh issue list --state open --limit 500 --search "no:milestone -label:quality-ci" --json number -q length
```

Expected: 8 milestones summing to 88 with non-empty descriptions; 39 unmilestoned; `now` at 7;
no cluster labels; `quality-ci` at 22; backlog at 18.

Criterion 3 deliberately lists **every** issue whose horizon count is not 1, including #839, so
its state is reported rather than silently skipped. Per the spec, #839 is the one permitted
exception — if any other number appears, fix it. If #839 does not appear, it still carries a
horizon label, which is harmless but means the exemption is untested.

- [ ] **Step 3: Record the outcome and commit the plan updates**

```bash
git add -A docs/superpowers/plans/
git commit -m "docs(triage): record issue organization migration outcome"
```

- [ ] **Step 4: Retire the rollback window**

Once criteria pass and the workflow has run clean for a day, note in the PR that `docs/superpowers/plans/triage-baseline-2026-08-12.json` is the restore point and may be deleted after the next release. Do not delete it during this task.
