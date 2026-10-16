#!/usr/bin/env node
import { writeFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const DEFAULT_REPO = process.env.GITHUB_REPOSITORY ?? process.env.GH_REPO ?? "gents-ai/gents";
const TOKEN = process.env.GITHUB_TOKEN;
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
// Fixed lists: restore only ever deletes names from these, never every live
// label/milestone absent from the snapshot — that would destroy labels or
// milestones a maintainer created after capture, and deleting a milestone is
// unrecoverable.
const INTRODUCED = ["quality-ci", "needs-triage"];
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

// Injectable so the restore guards are testable without a network.
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
      state: it.state,
      labels: it.labels.map((l) => l.name).sort(),
      milestone: it.milestone ? it.milestone.title : null,
    });
  };

  for (const it of await paginate(api, `/repos/${REPO}/issues?state=open`)) {
    if (it.pull_request) continue;
    record(it);
  }
  const openIssues = items.size;

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

  // Destructive tool: refuse a wrong target repository.
  if (snap.capturedFrom !== REPO) {
    throw new Error(
      `refusing to restore: snapshot was captured from ${snap.capturedFrom}, ` +
        `but the target repository is ${REPO}`,
    );
  }

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

  for (const it of snap.items) {
    await api(`/repos/${REPO}/issues/${it.number}/labels`, {
      method: "PUT",
      body: JSON.stringify({ labels: it.labels }),
    });
  }
  console.log(`restored labels on ${snap.items.length} items`);

  // Recreate captured milestones that no longer exist; deletion is
  // allow-listed to INTRODUCED_MILESTONES only.
  const known = new Map(snap.milestones.map((m) => [m.title, m]));
  const liveMilestones = await paginate(api, `/repos/${REPO}/milestones?state=all`);
  const liveTitles = new Set(liveMilestones.map((m) => m.title));

  for (const m of snap.milestones) {
    if (liveTitles.has(m.title)) continue;
    await api(`/repos/${REPO}/milestones`, {
      method: "POST",
      body: JSON.stringify({
        title: m.title,
        state: m.state ?? "open",
        description: m.description ?? "",
        // The API rejects an explicit null due_on.
        ...(m.due_on ? { due_on: m.due_on } : {}),
      }),
    });
    console.log(`recreated milestone: ${m.title}`);
  }

  // Delete only milestones this migration introduces and that were absent at
  // capture time; anything else is a maintainer's.
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

  // Restore each item's milestone, clearing it when none was captured. Ordered
  // after the deletion step so a captured title can never resolve to a
  // milestone that is about to be deleted.
  const byTitle = new Map(
    (await paginate(api, `/repos/${REPO}/milestones?state=all`)).map((m) => [m.title, m.number]),
  );
  let milestoned = 0;
  for (const it of snap.items) {
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

  // Restore the roadmap issue's title and body.
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
