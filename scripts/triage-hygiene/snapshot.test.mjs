// Tests for the rollback tool's milestone handling. `restore()` is the only
// destructive thing in this directory, and its milestone step could previously
// delete every milestone in the repository — including a maintainer's — while
// having no way to recreate any of them. These tests run against an in-memory
// GitHub; nothing here touches the network or performs a live write.
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

// Serves the endpoints restore() reads and records everything it writes.
const fakeApi = ({ milestones = [], labels = [] } = {}) => {
  const calls = [];
  const api = async (path, init = {}) => {
    const method = init.method ?? "GET";
    calls.push({ method, path, body: init.body ? JSON.parse(init.body) : undefined });
    const page = Number(new URL(`https://x${path}`).searchParams.get("page") ?? 1);
    if (path.includes("/milestones?")) return page === 1 ? milestones : [];
    if (path.includes("/labels?")) return page === 1 ? labels : [];
    return null;
  };
  return { api, calls };
};

// restore() narrates every step to stdout; capture it so the test output stays
// readable and so the log-and-skip line can be asserted on.
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
  calls.filter((c) => c.method === "DELETE" && /\/milestones\/\d+$/.test(c.path)).map((c) => c.path);
const created = (calls) =>
  calls.filter((c) => c.method === "POST" && c.path.endsWith("/milestones")).map((c) => c.body);

test("restore refuses to delete a milestone it did not capture and does not create", async () => {
  // The exact catastrophe: an empty `milestones` array in the snapshot.
  const { api, calls } = fakeApi({
    milestones: [
      { number: 1, title: "A maintainer's release", state: "open" },
      { number: 2, title: "Someone else's epic", state: "open" },
    ],
  });
  const logs = await withLogs(() => restore(api, snapshotFile(baseSnapshot()), REPO));

  assert.deepEqual(deleted(calls), [], "no milestone may be deleted");
  assert.equal(logs.filter((l) => l.includes("refusing to delete it")).length, 2);
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

test("an introduced milestone that existed at capture time is kept", async () => {
  const snap = baseSnapshot({
    milestones: [{ number: 9, title: "Gents cutover", state: "open", description: "", due_on: null }],
  });
  const { api, calls } = fakeApi({
    milestones: [{ number: 9, title: "Gents cutover", state: "open" }],
  });
  await withLogs(() => restore(api, snapshotFile(snap), REPO));
  assert.deepEqual(deleted(calls), [], "it predates the migration, so it is not ours to delete");
  assert.deepEqual(created(calls), [], "and it already exists, so nothing is recreated");
});

test("restore recreates a captured milestone that no longer exists, with its due date", async () => {
  const snap = baseSnapshot({
    milestones: [
      {
        number: 4,
        title: "Q3 planning",
        state: "closed",
        description: "the plan",
        due_on: "2026-09-30T07:00:00Z",
      },
    ],
  });
  const { api, calls } = fakeApi({ milestones: [] });
  await withLogs(() => restore(api, snapshotFile(snap), REPO));

  assert.deepEqual(created(calls), [
    {
      title: "Q3 planning",
      state: "closed",
      description: "the plan",
      due_on: "2026-09-30T07:00:00Z",
    },
  ]);
});

test("a captured milestone with no due date is recreated without a null due_on", async () => {
  const snap = baseSnapshot({
    milestones: [{ number: 4, title: "Q3 planning", state: "open", description: "", due_on: null }],
  });
  const { api, calls } = fakeApi({ milestones: [] });
  await withLogs(() => restore(api, snapshotFile(snap), REPO));
  assert.equal(created(calls).length, 1);
  assert.ok(!("due_on" in created(calls)[0]), "the API rejects an explicit null due_on");
});

test("restore still refuses to run against a different repository", async () => {
  const { api } = fakeApi();
  await assert.rejects(
    () => restore(api, snapshotFile(baseSnapshot({ capturedFrom: "someone/fork" })), REPO),
    /refusing to restore/,
  );
});
