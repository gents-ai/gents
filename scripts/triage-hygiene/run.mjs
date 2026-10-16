#!/usr/bin/env node
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
// A single renamed or stray `roadmap:` label turns the whole backlog
// non-compliant at once; cap notifications per run and fail rather than
// quietly spamming the backlog.
export const MAX_COMMENTS_PER_RUN = 10;
export const MAX_RETRIES = 3;
const MAX_BACKOFF_MS = 60_000;

// Only explicit negatives disable dry-run: this bot holds `issues: write` on
// the production tracker, so a typo'd truthy value must never enable live writes.
const DRY_RUN_RAW = (process.env.DRY_RUN ?? "").trim().toLowerCase();
const DRY_RUN =
  DRY_RUN_RAW !== "" && DRY_RUN_RAW !== "0" && DRY_RUN_RAW !== "false";

// A 403 is retryable only with rate-limit evidence; 404/422 are real answers.
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
  if (
    headers.get("x-ratelimit-remaining") === "0" &&
    Number.isFinite(reset) &&
    reset > 0
  ) {
    const wait = reset * 1000 - Date.now();
    if (wait > 0) return Math.min(wait, MAX_BACKOFF_MS);
  }
  return Math.min(1000 * 2 ** attempt, MAX_BACKOFF_MS);
};

export const makeApi =
  ({ token, fetchImpl = fetch, sleepImpl = sleep, warn = console.warn } = {}) =>
  async (path, init = {}) => {
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
      if (attempt >= MAX_RETRIES || !isRetryable(res.status, res.headers, body))
        throw err;

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

// Must be exhaustive: a prior conflict marker can sit on a later page, and
// missing it reposts the same conflict comment on every sweep.
const listComments = async (api, repo, number) =>
  (await paginate(api, `/repos/${repo}/issues/${number}/comments`)).map(
    (c) => ({
      id: c.id,
      body: c.body ?? "",
      author: c.user?.login ?? null,
    }),
  );

// The identities this run may delete comments as.
export const resolveBotLogins = async (api, warn = console.warn) => {
  const logins = new Set(DEFAULT_BOT_LOGINS);
  try {
    const me = await api("/user");
    if (me?.login) logins.add(me.login);
  } catch (err) {
    warn(
      `could not resolve the authenticated identity (${err.status ?? err.message}); ` +
        `comment deletion is limited to ${[...logins].join(", ")}`,
    );
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
  const stats = {
    examined: 0,
    changed: 0,
    failed: 0,
    commentsPosted: 0,
    commentsSuppressed: 0,
  };

  const applyTo = async (issue) => {
    const labels = issue.labels.map((l) =>
      typeof l === "string" ? l : l.name,
    );
    const context = { number: issue.number, labels, state: issue.state };
    // Fetch comments lazily; the pure module owns the condition.
    const comments = requiresCommentContext(context)
      ? await listComments(api, repo, issue.number)
      : [];

    const plan = reconcile({ ...context, comments, botLogins });
    if (
      !plan.add.length &&
      !plan.remove.length &&
      !plan.comment &&
      !plan.deleteComments.length
    ) {
      return false;
    }

    // Compute the budget in dry-run too, so a dry run predicts the truncation.
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
      await api(
        `/repos/${repo}/issues/${issue.number}/labels/${encodeURIComponent(name)}`,
        {
          method: "DELETE",
        },
      ).catch((err) => {
        // A 404 already means the desired end state; anything else must not
        // count as a successful reconcile.
        if (err.status !== 404) throw err;
      });
    }
    for (const id of plan.deleteComments) {
      await api(`/repos/${repo}/issues/comments/${id}`, {
        method: "DELETE",
      }).catch((err) => {
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
    // Single-issue mode fails loudly: it is driven by one event with nothing to
    // isolate a failure from.
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
        // One failure must not abort the rest of the sweep.
        stats.failed += 1;
        error(`#${issue.number}: FAILED ${err.message}`);
      }
    }
  }

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
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

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
  // A partially-completed sweep or exhausted comment budget is a failure.
  if (stats.failed > 0 || stats.commentsSuppressed > 0) process.exitCode = 1;
}
