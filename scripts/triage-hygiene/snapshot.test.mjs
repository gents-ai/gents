import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { restore } from "./snapshot.mjs";

const REPO = "gents-ai/gents";

const snapshotFile = (snap) => {
  const dir = mkdtempSync(join(tmpdir(), "triage-snap-"));
  const file = join(dir, "snap.json");
  writeFileSync(file, JSON.stringify(snap));
  return file;
};

const baseSnapshot = (overrides = {}) => ({
  capturedFrom: REPO,
  labels: [],
  milestones: [],
  items: [],
  roadmapIssue: { number: 839, title: "t", body: "b" },
  ...overrides,
});

const fakeApi = ({ milestones = [], labels = [] } = {}) => {
  const calls = [];
  const api = async (path, init = {}) => {
    const method = init.method ?? "GET";
    calls.push({
      method,
      path,
      body: init.body ? JSON.parse(init.body) : undefined,
    });
    const page = Number(
      new URL(`https://x${path}`).searchParams.get("page") ?? 1,
    );
    if (path.includes("/milestones?")) return page === 1 ? milestones : [];
    if (path.includes("/labels?")) return page === 1 ? labels : [];
    return null;
  };
  return { api, calls };
};

const withLogs = async (fn) => {
  const logs = [];
  const orig = console.log;
  console.log = (...a) => logs.push(a.join(" "));
  try {
    await fn();
  } finally {
    console.log = orig;
  }
  return logs;
};

const deleted = (calls) =>
  calls
    .filter((c) => c.method === "DELETE" && /\/milestones\/\d+$/.test(c.path))
    .map((c) => c.path);

test("restore refuses to delete a milestone it did not capture and does not create", async () => {
  const { api, calls } = fakeApi({
    milestones: [
      { number: 1, title: "A maintainer's release", state: "open" },
      { number: 2, title: "Someone else's epic", state: "open" },
    ],
  });
  const logs = await withLogs(() =>
    restore(api, snapshotFile(baseSnapshot()), REPO),
  );

  assert.deepEqual(deleted(calls), [], "no milestone may be deleted");
  assert.equal(
    logs.filter((l) => l.includes("refusing to delete it")).length,
    2,
  );
});

test("restore deletes only milestones this migration introduces", async () => {
  const { api, calls } = fakeApi({
    milestones: [
      { number: 1, title: "Gents cutover", state: "open" }, // introduced
      { number: 2, title: "iOS hardening", state: "open" }, // introduced
      { number: 3, title: "v2.0 release", state: "open" }, // a maintainer's
    ],
  });
  await withLogs(() => restore(api, snapshotFile(baseSnapshot()), REPO));

  assert.deepEqual(deleted(calls).sort(), [
    `/repos/${REPO}/milestones/1`,
    `/repos/${REPO}/milestones/2`,
  ]);
});

test("restore still refuses to run against a different repository", async () => {
  const { api } = fakeApi();
  await assert.rejects(
    () =>
      restore(
        api,
        snapshotFile(baseSnapshot({ capturedFrom: "someone/fork" })),
        REPO,
      ),
    /refusing to restore/,
  );
});
