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
