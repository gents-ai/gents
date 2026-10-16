// Fences the REST layer's destructive-safety paths: comment deletion wiring and
// the comment budget. Runs against an in-memory GitHub; nothing touches the
// network.
import { test } from "node:test";
import assert from "node:assert/strict";
import { run, makeApi, resolveBotLogins, MAX_COMMENTS_PER_RUN, MAX_RETRIES } from "./run.mjs";
import {
  conflictMarker,
  MISSING_HORIZON_MARKER,
  DEFAULT_BOT_LOGINS,
  NEEDS_TRIAGE,
} from "./reconcile.mjs";

const REPO = "gents-ai/gents";
const BOT = DEFAULT_BOT_LOGINS[0];

const issue = (number, labels = [], state = "open") => ({ number, state, labels });

// An in-memory GitHub covering exactly the endpoints run() calls. `failOn` is a
// predicate over (path, init) that makes a call throw, so a transient failure
// can be aimed at one issue.
const fakeApi = ({ issues = [], comments = {}, failOn = () => null } = {}) => {
  const calls = [];
  const api = async (path, init = {}) => {
    calls.push({ method: init.method ?? "GET", path, body: init.body });
    const boom = failOn(path, init);
    if (boom) throw boom;

    const page = Number(new URL(`https://x${path}`).searchParams.get("page") ?? 1);
    if (path.startsWith(`/repos/${REPO}/issues?state=open`)) {
      return page === 1 ? issues : [];
    }
    const commentsMatch = path.match(/^\/repos\/.+\/issues\/(\d+)\/comments\?/);
    if (commentsMatch) return page === 1 ? (comments[commentsMatch[1]] ?? []) : [];
    const singleMatch = path.match(/^\/repos\/.+\/issues\/(\d+)$/);
    if (singleMatch) {
      const found = issues.find((i) => String(i.number) === singleMatch[1]);
      if (!found) throw Object.assign(new Error("not found"), { status: 404 });
      return found;
    }
    return null;
  };
  return { api, calls };
};

const of = (calls, method, re) => calls.filter((c) => c.method === method && re.test(c.path));

test("the comment budget caps comments at MAX_COMMENTS_PER_RUN", async () => {
  const issues = Array.from({ length: 15 }, (_, i) => issue(100 + i, [{ name: "roadmap: nowe" }]));
  const { api, calls } = fakeApi({ issues });

  const stats = await run({ api, repo: REPO, log: () => {}, error: () => {} });

  assert.equal(stats.commentsPosted, MAX_COMMENTS_PER_RUN);
  assert.equal(stats.commentsSuppressed, 15 - MAX_COMMENTS_PER_RUN);
  assert.equal(of(calls, "POST", /\/comments$/).length, MAX_COMMENTS_PER_RUN);
  // Labels are idempotent and cheap, so they are NOT capped: all 15 get flagged.
  assert.equal(of(calls, "POST", /\/labels$/).length, 15);
  assert.equal(stats.examined, 15);
  assert.equal(stats.changed, 15);
});

test("the budget is enforced in dry-run too, so a dry run predicts the truncation", async () => {
  const issues = Array.from({ length: 13 }, (_, i) => issue(400 + i, []));
  const { api, calls } = fakeApi({ issues });
  const stats = await run({ api, repo: REPO, dryRun: true, log: () => {}, error: () => {} });

  assert.equal(stats.commentsSuppressed, 3);
  assert.equal(of(calls, "POST", /./).length, 0, "dry run writes nothing at all");
});

const response = (status, { body = "", headers = {}, json = null } = {}) => ({
  ok: status >= 200 && status < 300,
  status,
  headers: new Headers(headers),
  text: async () => body,
  json: async () => json,
});

const fakeFetch = (responses) => {
  const slept = [];
  let i = 0;
  const fetchImpl = async () => responses[Math.min(i++, responses.length - 1)];
  return { fetchImpl, slept, sleepImpl: async (ms) => slept.push(ms), calls: () => i };
};

test("api retries a 502 and succeeds", async () => {
  const f = fakeFetch([response(502, { body: "bad gateway" }), response(200, { json: { ok: 1 } })]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  assert.deepEqual(await api("/x"), { ok: 1 });
  assert.equal(f.calls(), 2);
  assert.deepEqual(f.slept, [1000]);
});

test("api honours Retry-After on a 429", async () => {
  const f = fakeFetch([
    response(429, { headers: { "retry-after": "7" } }),
    response(200, { json: {} }),
  ]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  await api("/x");
  assert.deepEqual(f.slept, [7000]);
});

test("api does NOT retry a plain 403 — that is a real permissions answer", async () => {
  const f = fakeFetch([response(403, { body: "Resource not accessible by integration" })]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  await assert.rejects(() => api("/x"), /403/);
  assert.equal(f.calls(), 1);
});

test("api surfaces the status on the thrown error so callers can treat 404 as done", async () => {
  const f = fakeFetch([response(404, { body: "gone" })]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  const err = await api("/x").catch((e) => e);
  assert.equal(err.status, 404);
});

// The runner must delete only what reconcile() nominated.
test("the runner deletes the bot's stale conflict comment when an issue goes clean", async () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const { api, calls } = fakeApi({
    issues: [issue(7, [{ name: "roadmap: now" }, { name: NEEDS_TRIAGE }])],
    comments: { 7: [{ id: 555, body: `x ${stale}`, user: { login: BOT } }] },
  });
  await run({ api, repo: REPO, botLogins: [BOT], log: () => {}, error: () => {} });

  assert.equal(of(calls, "DELETE", /\/issues\/comments\/555$/).length, 1);
  assert.equal(of(calls, "DELETE", /\/labels\/needs-triage$/).length, 1);
  assert.equal(of(calls, "POST", /./).length, 0, "nothing is added or commented");
});

test("the runner retracts both notice types but leaves the human's comment", async () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const { api, calls } = fakeApi({
    issues: [issue(14, [{ name: "roadmap: now" }, { name: NEEDS_TRIAGE }])],
    comments: {
      14: [
        { id: 601, body: `welcome ${MISSING_HORIZON_MARKER}`, user: { login: BOT } },
        { id: 602, body: `quoting ${MISSING_HORIZON_MARKER}`, user: { login: "a-human" } },
        { id: 603, body: `conflict ${stale}`, user: { login: BOT } },
      ],
    },
  });
  await run({ api, repo: REPO, botLogins: [BOT], log: () => {}, error: () => {} });

  assert.deepEqual(
    of(calls, "DELETE", /\/issues\/comments\/\d+$/).map((c) => c.path.split("/").pop()),
    ["601", "603"],
  );
});

test("the runner never deletes a human's comment carrying the marker", async () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const { api, calls } = fakeApi({
    issues: [issue(8, [{ name: "roadmap: now" }, { name: NEEDS_TRIAGE }])],
    comments: { 8: [{ id: 556, body: `x ${stale}`, user: { login: "a-human" } }] },
  });
  await run({ api, repo: REPO, botLogins: [BOT], log: () => {}, error: () => {} });
  assert.equal(of(calls, "DELETE", /\/issues\/comments\//).length, 0);
});

test("single-issue mode clears needs-triage from a closed issue", async () => {
  const { api, calls } = fakeApi({
    issues: [issue(11, [{ name: "bug" }, { name: NEEDS_TRIAGE }], "closed")],
  });
  const stats = await run({ api, repo: REPO, issueNumber: 11, log: () => {}, error: () => {} });

  assert.equal(stats.changed, 1);
  assert.equal(of(calls, "DELETE", /\/labels\/needs-triage$/).length, 1);
  assert.equal(of(calls, "POST", /./).length, 0, "a closed issue is never flagged or commented on");
  assert.equal(of(calls, "GET", /\/comments\?/).length, 0);
});

test("resolveBotLogins falls back to the bot login when GET /user is forbidden", async () => {
  const warned = [];
  const logins = await resolveBotLogins(async () => {
    throw Object.assign(new Error("403"), { status: 403 });
  }, (...a) => warned.push(a.join(" ")));
  assert.deepEqual(logins, DEFAULT_BOT_LOGINS);
  assert.equal(warned.length, 1);
});
