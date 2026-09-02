#!/usr/bin/env node
// REST layer for the triage-hygiene bot. Every decision comes from
// reconcile.mjs — nothing in this file re-derives a rule, because a runner and
// a decision function encoding the same rule have already drifted apart here
// once.
import { setTimeout as sleep } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import {
  reconcile,
  requiresCommentContext,
  DEFAULT_BOT_LOGINS,
  NEEDS_TRIAGE,
} from "./reconcile.mjs";

const REPO = process.env.GITHUB_REPOSITORY ?? "gents-ai/gents";
const TOKEN = process.env.GITHUB_TOKEN;
// Labels are idempotent and cheap; comments are permanent and mail everyone
// watching. A single renamed or stray `roadmap:` label turns the whole backlog
// non-compliant at once, so cap the notifications one run can generate and fail
// the run rather than quietly spamming ~126 issues unattended.
export const MAX_COMMENTS_PER_RUN = 10;
// Bounded retry for transient GitHub failures. Three is enough to ride out a
// 502 or a secondary-rate-limit pause without turning a real outage into a
// half-hour hang.
export const MAX_RETRIES = 3;
const MAX_BACKOFF_MS = 60_000;

// Deliberately permissive: this bot holds `issues: write` on the production
// tracker, so a typo'd truthy value (`DRY_RUN=true`, `DRY_RUN=yes`) must never
// silently enable live writes. Only the explicit negatives disable dry-run;
// an unset variable means disabled.
const DRY_RUN_RAW = (process.env.DRY_RUN ?? "").trim().toLowerCase();
const DRY_RUN = DRY_RUN_RAW !== "" && DRY_RUN_RAW !== "0" && DRY_RUN_RAW !== "false";

// A 404 or 422 is a real answer and retrying cannot change it. A 403 is
// ambiguous: it is both "you may not" and GitHub's secondary rate limit, so it
// is retryable only when it carries rate-limit evidence.
const isRetryable = (status, headers, body) => {
  if (status === 429 || status >= 500) return true;
  if (status !== 403) return false;
  return (
    headers.has("retry-after") ||
    headers.get("x-ratelimit-remaining") === "0" ||
    /rate limit/i.test(body)
  );
};

const retryDelayMs = (headers, attempt) => {
  const retryAfter = Number(headers.get("retry-after"));
  if (Number.isFinite(retryAfter) && retryAfter > 0) {
    return Math.min(retryAfter * 1000, MAX_BACKOFF_MS);
  }
  const reset = Number(headers.get("x-ratelimit-reset"));
  if (headers.get("x-ratelimit-remaining") === "0" && Number.isFinite(reset) && reset > 0) {
    const wait = reset * 1000 - Date.now();
    if (wait > 0) return Math.min(wait, MAX_BACKOFF_MS);
  }
  return Math.min(1000 * 2 ** attempt, MAX_BACKOFF_MS);
};

// Injectable so the retry policy is unit-testable without a network.
export const makeApi = ({
  token,
  fetchImpl = fetch,
  sleepImpl = sleep,
  warn = console.warn,
} = {}) => async (path, init = {}) => {
  const method = init.method ?? "GET";
  for (let attempt = 0; ; attempt += 1) {
    const res = await fetchImpl(`https://api.github.com${path}`, {
      ...init,
      headers: {
        accept: "application/vnd.github+json",
        authorization: `Bearer ${token}`,
        "x-github-api-version": "2022-11-28",
        ...(init.body ? { "content-type": "application/json" } : {}),
      },
    });
    if (res.ok) return res.status === 204 ? null : res.json();

    const body = await res.text();
    const err = new Error(`${method} ${path} -> ${res.status} ${body}`);
    err.status = res.status;
    if (attempt >= MAX_RETRIES || !isRetryable(res.status, res.headers, body)) throw err;

    const delay = retryDelayMs(res.headers, attempt);
    warn(
      `${method} ${path} -> ${res.status}; retrying in ${delay}ms ` +
        `(attempt ${attempt + 1}/${MAX_RETRIES})`,
    );
    await sleepImpl(delay);
  }
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

// Must be exhaustive, not just the first page: GitHub returns issue comments
// oldest-first, so on an issue with more than 100 comments a prior conflict
// marker sits on a later page. Missing it defeats the dedup check and reposts
// the same conflict comment on every sweep, forever.
const listComments = async (api, repo, number) =>
  (await paginate(api, `/repos/${repo}/issues/${number}/comments`)).map((c) => ({
    id: c.id,
    body: c.body ?? "",
    author: c.user?.login ?? null,
  }));

// The identities this run may delete comments as. `GET /user` is the identity
// actually holding the token; `github-actions[bot]` is what the workflow acts
// as. Both are this bot, and no other login is ever added.
export const resolveBotLogins = async (api, warn = console.warn) => {
  const logins = new Set(DEFAULT_BOT_LOGINS);
  try {
    const me = await api("/user");
    if (me?.login) logins.add(me.login);
  } catch (err) {
    warn(`could not resolve the authenticated identity (${err.status ?? err.message}); ` +
      `comment deletion is limited to ${[...logins].join(", ")}`);
  }
  return [...logins];
};

export const run = async ({
  api,
  repo = REPO,
  dryRun = false,
  issueNumber = undefined,
  botLogins = DEFAULT_BOT_LOGINS,
  log = console.log,
  error = console.error,
}) => {
  const stats = { examined: 0, changed: 0, failed: 0, commentsPosted: 0, commentsSuppressed: 0 };

  const applyTo = async (issue) => {
    const labels = issue.labels.map((l) => (typeof l === "string" ? l : l.name));
    const context = { number: issue.number, labels, state: issue.state };
    // Comments are only needed to dedupe notices or to find this bot's own
    // stale ones, so fetch them lazily — the pure module owns that condition.
    const comments = requiresCommentContext(context)
      ? await listComments(api, repo, issue.number)
      : [];

    const plan = reconcile({ ...context, comments, botLogins });
    if (!plan.add.length && !plan.remove.length && !plan.comment && !plan.deleteComments.length) {
      return false;
    }

    // Decide the budget in dry-run too, so a dry run surfaces the same
    // truncation a live run would hit instead of hiding it.
    let willComment = false;
    if (plan.comment) {
      if (stats.commentsPosted < MAX_COMMENTS_PER_RUN) {
        willComment = true;
        stats.commentsPosted += 1;
      } else {
        stats.commentsSuppressed += 1;
      }
    }

    log(
      `#${issue.number}: +[${plan.add}] -[${plan.remove}] comment=${willComment}` +
        `${plan.comment && !willComment ? " (SUPPRESSED: comment budget exhausted)" : ""}` +
        ` delete=[${plan.deleteComments}]`,
    );
    if (dryRun) return true;

    if (plan.add.length) {
      await api(`/repos/${repo}/issues/${issue.number}/labels`, {
        method: "POST",
        body: JSON.stringify({ labels: plan.add }),
      });
    }
    for (const name of plan.remove) {
      await api(`/repos/${repo}/issues/${issue.number}/labels/${encodeURIComponent(name)}`, {
        method: "DELETE",
      }).catch((err) => {
        // A 404 means the label is already gone, which is the desired end state.
        // Anything else — permissions, rate limits, 5xx — must not be reported
        // as a successful reconcile.
        if (err.status !== 404) throw err;
      });
    }
    for (const id of plan.deleteComments) {
      await api(`/repos/${repo}/issues/comments/${id}`, { method: "DELETE" }).catch((err) => {
        // Already deleted is the desired end state.
        if (err.status !== 404) throw err;
      });
    }
    if (willComment) {
      await api(`/repos/${repo}/issues/${issue.number}/comments`, {
        method: "POST",
        body: JSON.stringify({ body: plan.comment }),
      });
    }
    return true;
  };

  if (issueNumber) {
    // Single-issue mode fails loudly: it is driven by one event, there is
    // nothing to isolate a failure from, and a swallowed error would look like
    // a successful reconcile. Closed issues are reconciled here (the flag is
    // cleared) — the sweep deliberately never enumerates them.
    const issue = await api(`/repos/${repo}/issues/${issueNumber}`);
    if (!issue.pull_request) {
      stats.examined += 1;
      if (await applyTo(issue)) stats.changed += 1;
    }
  } else {
    const issues = await paginate(api, `/repos/${repo}/issues?state=open`);
    for (const issue of issues) {
      if (issue.state !== "open" || issue.pull_request) continue;
      stats.examined += 1;
      try {
        if (await applyTo(issue)) stats.changed += 1;
      } catch (err) {
        // One transient 502 must never cost the other 127 issues their sweep.
        stats.failed += 1;
        error(`#${issue.number}: FAILED ${err.message}`);
      }
    }
  }

  // Distinguish intent from effect: nothing was written in dry-run mode.
  log(
    `${NEEDS_TRIAGE} reconcile complete: ${stats.examined} examined, ` +
      `${stats.changed} ${dryRun ? "would change" : "changed"}, ${stats.failed} failed`,
  );
  if (stats.commentsSuppressed > 0) {
    error(
      `COMMENT BUDGET EXHAUSTED: ${MAX_COMMENTS_PER_RUN} comments is the per-run maximum and ` +
        `${stats.commentsSuppressed} issue(s) still need one. This usually means a \`roadmap:\` ` +
        `label was renamed or a stray one was created — fix the labels, then re-run.`,
    );
  }
  return stats;
};

const isMain =
  process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  if (!TOKEN) {
    console.error("GITHUB_TOKEN is required");
    process.exit(1);
  }
  const api = makeApi({ token: TOKEN });
  const stats = await run({
    api,
    repo: REPO,
    dryRun: DRY_RUN,
    issueNumber: process.env.ISSUE_NUMBER,
    botLogins: await resolveBotLogins(api),
  });
  // A partially-completed sweep, or one that ran out of comment budget, is a
  // failure: it must show red rather than look like success.
  if (stats.failed > 0 || stats.commentsSuppressed > 0) process.exitCode = 1;
}
