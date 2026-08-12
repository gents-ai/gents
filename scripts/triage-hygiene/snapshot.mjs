#!/usr/bin/env node
// Capture and restore every piece of state the triage migration destroys.
//
// Deleting a label removes it from EVERY associated item, including closed
// issues and pull requests, and discards the label's own definition. Renaming
// is likewise not self-inverting. So the snapshot records label definitions,
// the full label set and milestone of every open issue plus every item
// carrying a doomed label in any state, existing milestones (with their due
// dates, so restore can recreate them faithfully), and #839's title and body.
import { writeFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

// GITHUB_REPOSITORY wins when both are set: it is the name Actions and run.mjs
// use, so an ambient GH_REPO left over in a shell cannot redirect a restore.
const DEFAULT_REPO = process.env.GITHUB_REPOSITORY ?? process.env.GH_REPO ?? "source-inc/gents";
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
// Milestones this migration introduces, by title. Restore may delete ONLY
// these, and only when they were absent at capture time. The baseline snapshot
// captured zero milestones, so "delete everything not in the snapshot" would
// delete every milestone in the repository — including any a maintainer created
// after capture. Deleting a maintainer's milestone is unrecoverable (it
// detaches from every issue that carried it); leaving one behind is a two-click
// fix. Same fixed-list reasoning as INTRODUCED above.
const INTRODUCED_MILESTONES = [
  "Authority and provenance hardening",
  "Fleet convergence and P2P durability",
  "Multi-agent coordination",
  "Long-context correctness",
  "Durable trace, attribution and trust",
  "Provider fidelity and rig removal",
  "iOS hardening",
  "Gents cutover",
];
const ROADMAP_ISSUE = 839;

// Injectable so the restore guards are unit-testable without a network and
// without ever performing a live write.
export const makeApi = (token) => async (path, init = {}) => {
  const res = await fetch(`https://api.github.com${path}`, {
    ...init,
    headers: {
      accept: "application/vnd.github+json",
      authorization: `Bearer ${token}`,
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

const paginate = async (api, path) => {
  const out = [];
  for (let page = 1; ; page += 1) {
    const sep = path.includes("?") ? "&" : "?";
    const batch = await api(`${path}${sep}per_page=100&page=${page}`);
    out.push(...batch);
    if (batch.length < 100) break;
  }
  return out;
};

export const capture = async (api, file, REPO = DEFAULT_REPO) => {
  const labels = (await paginate(api, `/repos/${REPO}/labels`)).map((l) => ({
    name: l.name,
    color: l.color,
    description: l.description ?? "",
  }));

  // `due_on` is captured because restore recreates milestones it finds
  // missing, and a milestone recreated without its due date is not the same
  // milestone: the date drives every "what is late" view in the UI.
  const milestones = (await paginate(api, `/repos/${REPO}/milestones?state=all`)).map((m) => ({
    number: m.number,
    title: m.title,
    state: m.state,
    description: m.description ?? "",
    due_on: m.due_on ?? null,
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
  for (const it of await paginate(api, `/repos/${REPO}/issues?state=open`)) {
    if (it.pull_request) continue;
    record(it);
  }
  const openIssues = items.size;

  // Any state, so closed issues and pull requests are covered too. The Map
  // deduplicates against the open-issue sweep above by issue number.
  for (const label of DOOMED) {
    const hits = await paginate(
      api,
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

export const restore = async (api, file, REPO = DEFAULT_REPO) => {
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
  const live = new Set((await paginate(api, `/repos/${REPO}/labels`)).map((l) => l.name));
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

  // 4. Reconcile milestones in both directions, but asymmetrically: recreation
  // is unconditional, deletion is allow-listed.
  const known = new Map(snap.milestones.map((m) => [m.title, m]));
  const liveMilestones = await paginate(api, `/repos/${REPO}/milestones?state=all`);
  const liveTitles = new Set(liveMilestones.map((m) => m.title));

  // 4a. Recreate captured milestones that no longer exist. Without this the
  // tool could only ever destroy state, never return it.
  for (const m of snap.milestones) {
    if (liveTitles.has(m.title)) continue;
    await api(`/repos/${REPO}/milestones`, {
      method: "POST",
      body: JSON.stringify({
        title: m.title,
        state: m.state ?? "open",
        description: m.description ?? "",
        // Omit rather than send null: the API rejects an explicit null here.
        ...(m.due_on ? { due_on: m.due_on } : {}),
      }),
    });
    console.log(`recreated milestone: ${m.title}`);
  }

  // 4b. Delete only milestones this migration is known to create and that were
  // absent at capture time. Anything else is a maintainer's — log and skip it.
  const introduced = new Set(INTRODUCED_MILESTONES);
  for (const m of liveMilestones) {
    if (known.has(m.title)) continue;
    if (!introduced.has(m.title)) {
      console.log(
        `skipped unknown milestone "${m.title}": not captured and not one this ` +
          `migration creates, so it is a maintainer's — refusing to delete it`,
      );
      continue;
    }
    await api(`/repos/${REPO}/milestones/${m.number}`, { method: "DELETE" });
    console.log(`deleted introduced milestone: ${m.title}`);
  }

  // 5. Restore each item's milestone, clearing it when none was captured.
  // Ordered after the deletion step so a captured title can never resolve to a
  // milestone that is about to be deleted.
  const byTitle = new Map(
    (await paginate(api, `/repos/${REPO}/milestones?state=all`)).map((m) => [m.title, m.number]),
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

const isMain =
  process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const [mode, file] = process.argv.slice(2);
  if (!file || (mode !== "capture" && mode !== "restore")) {
    console.error("usage: snapshot.mjs <capture|restore> <file>");
    process.exit(1);
  }
  if (!TOKEN) {
    console.error("GITHUB_TOKEN is required");
    process.exit(1);
  }
  const api = makeApi(TOKEN);
  await (mode === "capture" ? capture(api, file) : restore(api, file));
}
