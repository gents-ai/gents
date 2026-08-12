#!/usr/bin/env node
// Capture and restore every piece of state the triage migration destroys.
//
// Deleting a label removes it from EVERY associated item, including closed
// issues and pull requests, and discards the label's own definition. Renaming
// is likewise not self-inverting. So the snapshot records label definitions,
// the full label set of every item carrying a doomed label in any state,
// existing milestones, and #839's title and body.
import { writeFileSync, readFileSync } from "node:fs";

const REPO = process.env.GH_REPO ?? "source-inc/gents";
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

  // Any state, so closed issues and pull requests are covered too.
  const items = new Map();
  for (const label of DOOMED) {
    const hits = await paginate(
      `/repos/${REPO}/issues?state=all&labels=${encodeURIComponent(label)}`,
    );
    for (const it of hits) {
      items.set(it.number, {
        number: it.number,
        isPullRequest: Boolean(it.pull_request),
        state: it.state,
        labels: it.labels.map((l) => l.name).sort(),
        milestone: it.milestone ? it.milestone.title : null,
      });
    }
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
      `${snap.items.length} affected items (incl. closed and PRs), #${ROADMAP_ISSUE} -> ${file}`,
  );
};

const restore = async (file) => {
  const snap = JSON.parse(readFileSync(file, "utf8"));

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

  // 2. Restore each affected item's full label set.
  for (const it of snap.items) {
    await api(`/repos/${REPO}/issues/${it.number}/labels`, {
      method: "PUT",
      body: JSON.stringify({ labels: it.labels }),
    });
  }
  console.log(`restored labels on ${snap.items.length} items`);

  // 3. Delete milestones that did not exist at capture time.
  const known = new Set(snap.milestones.map((m) => m.title));
  for (const m of await paginate(`/repos/${REPO}/milestones?state=all`)) {
    if (known.has(m.title)) continue;
    await api(`/repos/${REPO}/milestones/${m.number}`, { method: "DELETE" });
    console.log(`deleted milestone: ${m.title}`);
  }

  // 4. Restore the roadmap issue's title and body.
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
