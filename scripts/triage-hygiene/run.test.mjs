// Tests for the REST layer's robustness properties — the comment budget, the
// per-issue error isolation, and the retry policy. These paths have never been
// exercised in production, which is exactly why they need a fence: the error
// branch in particular is unreachable in any dry run.
//
// `run()` takes its `api` as a parameter and `makeApi()` takes its `fetch`, so
// every test here runs against an in-memory GitHub. Nothing in this file
// touches the network.
import { test } from "node:test";
import assert from "node:assert/strict";
import { run, makeApi, resolveBotLogins, MAX_COMMENTS_PER_RUN, MAX_RETRIES } from "./run.mjs";
import { conflictMarker, DEFAULT_BOT_LOGINS, NEEDS_TRIAGE } from "./reconcile.mjs";

const REPO = "source-inc/gents";
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
    return null; // POST/DELETE label, comment, delete-comment
  };
  return { api, calls };
};

const capture = () => {
  const lines = [];
  return { lines, sink: (...args) => lines.push(args.join(" ")) };
};

const of = (calls, method, re) => calls.filter((c) => c.method === method && re.test(c.path));

// ---------------------------------------------------------------------------
// H1 — the per-run comment budget.
// ---------------------------------------------------------------------------

test("the comment budget caps comments at MAX_COMMENTS_PER_RUN", async () => {
  // The renamed-label scenario: every issue is suddenly non-compliant at once.
  const issues = Array.from({ length: 15 }, (_, i) => issue(100 + i, [{ name: "roadmap: nowe" }]));
  const { api, calls } = fakeApi({ issues });
  const log = capture();
  const error = capture();

  const stats = await run({ api, repo: REPO, log: log.sink, error: error.sink });

  assert.equal(stats.commentsPosted, MAX_COMMENTS_PER_RUN);
  assert.equal(stats.commentsSuppressed, 15 - MAX_COMMENTS_PER_RUN);
  assert.equal(of(calls, "POST", /\/comments$/).length, MAX_COMMENTS_PER_RUN);
  // Labels are idempotent and cheap, so they are NOT capped: all 15 get flagged.
  assert.equal(of(calls, "POST", /\/labels$/).length, 15);
  assert.equal(stats.examined, 15);
  assert.equal(stats.changed, 15);
});

test("budget exhaustion logs one loud line naming how many issues still need a comment", async () => {
  const issues = Array.from({ length: 12 }, (_, i) => issue(200 + i, []));
  const { api } = fakeApi({ issues });
  const log = capture();
  const error = capture();

  const stats = await run({ api, repo: REPO, log: log.sink, error: error.sink });

  const loud = error.lines.filter((l) => l.includes("COMMENT BUDGET EXHAUSTED"));
  assert.equal(loud.length, 1, "exactly one loud line");
  assert.match(loud[0], /2 issue\(s\) still need one/);
  assert.match(loud[0], new RegExp(String(MAX_COMMENTS_PER_RUN)));
  // The summary still prints; truncation does not swallow it.
  assert.ok(log.lines.some((l) => l.includes("reconcile complete")));
  // Non-zero exit is driven off these counters by the entrypoint.
  assert.ok(stats.commentsSuppressed > 0);
});

test("suppressed issues are named in the per-issue log line", async () => {
  const issues = Array.from({ length: 11 }, (_, i) => issue(300 + i, []));
  const { api } = fakeApi({ issues });
  const log = capture();
  const stats = await run({ api, repo: REPO, log: log.sink, error: () => {} });

  assert.equal(stats.commentsSuppressed, 1);
  const suppressed = log.lines.filter((l) => l.includes("SUPPRESSED"));
  assert.equal(suppressed.length, 1);
  assert.match(suppressed[0], /#310/);
});

test("the budget is enforced in dry-run too, so a dry run predicts the truncation", async () => {
  const issues = Array.from({ length: 13 }, (_, i) => issue(400 + i, []));
  const { api, calls } = fakeApi({ issues });
  const stats = await run({ api, repo: REPO, dryRun: true, log: () => {}, error: () => {} });

  assert.equal(stats.commentsSuppressed, 3);
  assert.equal(of(calls, "POST", /./).length, 0, "dry run writes nothing at all");
});

test("a run inside the budget suppresses nothing", async () => {
  const issues = Array.from({ length: MAX_COMMENTS_PER_RUN }, (_, i) => issue(500 + i, []));
  const { api } = fakeApi({ issues });
  const stats = await run({ api, repo: REPO, log: () => {}, error: () => {} });
  assert.equal(stats.commentsSuppressed, 0);
  assert.equal(stats.commentsPosted, MAX_COMMENTS_PER_RUN);
});

// ---------------------------------------------------------------------------
// H2 — per-issue error isolation.
// ---------------------------------------------------------------------------

test("a transient failure on one issue does not abort the sweep", async () => {
  const issues = [1, 2, 3, 4, 5].map((n) => issue(n, [{ name: "bug" }]));
  const { api, calls } = fakeApi({
    issues,
    failOn: (path, init) =>
      init.method === "POST" && path === `/repos/${REPO}/issues/3/labels`
        ? Object.assign(new Error("502 Bad Gateway"), { status: 502 })
        : null,
  });
  const log = capture();
  const error = capture();

  const stats = await run({ api, repo: REPO, log: log.sink, error: error.sink });

  assert.equal(stats.examined, 5, "every issue is still examined");
  assert.equal(stats.failed, 1);
  assert.equal(stats.changed, 4);
  // The issues AFTER the failure were still processed — this is the whole point.
  assert.ok(of(calls, "POST", /\/issues\/4\/labels$/).length === 1);
  assert.ok(of(calls, "POST", /\/issues\/5\/labels$/).length === 1);
  assert.ok(error.lines.some((l) => l.includes("#3: FAILED") && l.includes("502")));
});

test("the summary always prints, including the failed count", async () => {
  const issues = [1, 2].map((n) => issue(n, [{ name: "bug" }]));
  const { api } = fakeApi({
    issues,
    failOn: (path, init) => (init.method === "POST" ? new Error("boom") : null),
  });
  const log = capture();
  const stats = await run({ api, repo: REPO, log: log.sink, error: () => {} });

  assert.equal(stats.failed, 2);
  const summary = log.lines.find((l) => l.includes("reconcile complete"));
  assert.ok(summary, "the summary line must print even when everything failed");
  assert.match(summary, /2 examined, 0 changed, 2 failed/);
});

test("every issue failing still reports every issue", async () => {
  const issues = Array.from({ length: 8 }, (_, i) => issue(600 + i, [{ name: "bug" }]));
  const { api } = fakeApi({
    issues,
    // Everything after the initial listing fails. (The listing itself is not
    // inside the per-issue catch by design: with no issue list there is no
    // sweep to isolate, and the run must fail outright.)
    failOn: (path) => (path.includes("state=open") ? null : new Error("total outage")),
  });
  const stats = await run({ api, repo: REPO, log: () => {}, error: () => {} });
  // The list call itself is not covered by the per-issue catch, but each
  // per-issue comment fetch is, so all 8 are counted as failures rather than
  // one throw ending the run.
  assert.equal(stats.failed, 8);
  assert.equal(stats.examined, 8);
});

test("single-issue mode still fails loudly", async () => {
  const { api } = fakeApi({
    issues: [issue(42, [{ name: "bug" }])],
    failOn: (_p, init) => (init.method === "POST" ? new Error("boom") : null),
  });
  await assert.rejects(
    () => run({ api, repo: REPO, issueNumber: 42, log: () => {}, error: () => {} }),
    /boom/,
    "an error in single-issue mode must propagate, not be counted and swallowed",
  );
});

// ---------------------------------------------------------------------------
// H2 — bounded retry with backoff in api().
// ---------------------------------------------------------------------------

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

test("api retries a secondary-rate-limit 403 identified by its body", async () => {
  const f = fakeFetch([
    response(403, { body: "You have exceeded a secondary rate limit" }),
    response(200, { json: {} }),
  ]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  await api("/x");
  assert.equal(f.calls(), 2);
});

test("api retries a 403 carrying Retry-After", async () => {
  const f = fakeFetch([
    response(403, { body: "forbidden", headers: { "retry-after": "3" } }),
    response(200, { json: {} }),
  ]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  await api("/x");
  assert.deepEqual(f.slept, [3000]);
});

test("api does NOT retry a plain 403 — that is a real permissions answer", async () => {
  const f = fakeFetch([response(403, { body: "Resource not accessible by integration" })]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  await assert.rejects(() => api("/x"), /403/);
  assert.equal(f.calls(), 1);
});

test("api does NOT retry 404 or 422 — they are real answers", async () => {
  for (const status of [404, 422]) {
    const f = fakeFetch([response(status, { body: "nope" })]);
    const api = makeApi({
      token: "t",
      fetchImpl: f.fetchImpl,
      sleepImpl: f.sleepImpl,
      warn: () => {},
    });
    await assert.rejects(() => api("/x"), new RegExp(String(status)));
    assert.equal(f.calls(), 1, `${status} must not be retried`);
  }
});

test("api gives up after MAX_RETRIES with exponential backoff", async () => {
  const f = fakeFetch([response(500, { body: "server error" })]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  await assert.rejects(() => api("/x"), /500/);
  assert.equal(f.calls(), MAX_RETRIES + 1, "one initial attempt plus MAX_RETRIES retries");
  assert.deepEqual(f.slept, [1000, 2000, 4000]);
});

test("api surfaces the status on the thrown error so callers can treat 404 as done", async () => {
  const f = fakeFetch([response(404, { body: "gone" })]);
  const api = makeApi({ token: "t", fetchImpl: f.fetchImpl, sleepImpl: f.sleepImpl, warn: () => {} });
  const err = await api("/x").catch((e) => e);
  assert.equal(err.status, 404);
});

// ---------------------------------------------------------------------------
// H4 wiring — the runner must delete only what reconcile() nominated.
// ---------------------------------------------------------------------------

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

test("the runner never deletes a human's comment carrying the marker", async () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const { api, calls } = fakeApi({
    issues: [issue(8, [{ name: "roadmap: now" }, { name: NEEDS_TRIAGE }])],
    comments: { 8: [{ id: 556, body: `x ${stale}`, user: { login: "a-human" } }] },
  });
  await run({ api, repo: REPO, botLogins: [BOT], log: () => {}, error: () => {} });
  assert.equal(of(calls, "DELETE", /\/issues\/comments\//).length, 0);
});

test("a clean unflagged issue does not fetch comments at all", async () => {
  const { api, calls } = fakeApi({ issues: [issue(9, [{ name: "roadmap: now" }])] });
  const stats = await run({ api, repo: REPO, log: () => {}, error: () => {} });
  assert.equal(stats.changed, 0);
  assert.equal(of(calls, "GET", /\/comments\?/).length, 0, "no fetch churn");
});

test("the runner tolerates a 404 when deleting an already-gone comment", async () => {
  const stale = conflictMarker(["roadmap: now", "roadmap: later"]);
  const { api } = fakeApi({
    issues: [issue(10, [{ name: "roadmap: now" }, { name: NEEDS_TRIAGE }])],
    comments: { 10: [{ id: 557, body: stale, user: { login: BOT } }] },
    failOn: (path, init) =>
      init.method === "DELETE" && path.includes("/issues/comments/")
        ? Object.assign(new Error("404"), { status: 404 })
        : null,
  });
  const stats = await run({ api, repo: REPO, log: () => {}, error: () => {} });
  assert.equal(stats.failed, 0);
});

// ---------------------------------------------------------------------------
// H6 wiring — closed issues.
// ---------------------------------------------------------------------------

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

test("the sweep never enumerates closed issues", async () => {
  const { api, calls } = fakeApi({ issues: [issue(12, [{ name: "bug" }], "closed")] });
  const stats = await run({ api, repo: REPO, log: () => {}, error: () => {} });
  assert.equal(stats.examined, 0);
  assert.ok(calls.every((c) => !c.path.includes("state=closed")));
});

test("pull requests are skipped in both modes", async () => {
  const pr = { ...issue(13, [{ name: "bug" }]), pull_request: { url: "x" } };
  const sweep = fakeApi({ issues: [pr] });
  assert.equal((await run({ api: sweep.api, repo: REPO, log: () => {}, error: () => {} })).examined, 0);
  const single = fakeApi({ issues: [pr] });
  const stats = await run({
    api: single.api,
    repo: REPO,
    issueNumber: 13,
    log: () => {},
    error: () => {},
  });
  assert.equal(stats.examined, 0);
});

// ---------------------------------------------------------------------------
// Bot identity resolution.
// ---------------------------------------------------------------------------

test("resolveBotLogins adds the authenticated user to the default bot login", async () => {
  const logins = await resolveBotLogins(async () => ({ login: "octocat" }));
  assert.deepEqual(logins.sort(), [BOT, "octocat"].sort());
});

test("resolveBotLogins falls back to the bot login when GET /user is forbidden", async () => {
  const warned = capture();
  const logins = await resolveBotLogins(async () => {
    throw Object.assign(new Error("403"), { status: 403 });
  }, warned.sink);
  assert.deepEqual(logins, DEFAULT_BOT_LOGINS);
  assert.equal(warned.lines.length, 1);
});
