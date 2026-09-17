import assert from "node:assert/strict";
import test from "node:test";
import {
  HostEnvironment,
  archiveContainerDirectory,
  hostMemoryPlan,
  validateRuntimeRevision,
} from "./host-environment.mjs";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

test("runtime provenance rejects stale, dirty, missing, or abbreviated revisions", () => {
  const commit = "a".repeat(40);
  const labels = {
    "org.opencontainers.image.revision": commit,
    "gents.eval.source-dirty": "false",
  };
  const revision = { commit, dirty: false };
  assert.doesNotThrow(() => validateRuntimeRevision(labels, revision));
  for (const source of [
    null,
    { commit, dirty: true },
    { commit: "aaaa", dirty: false },
  ])
    assert.throws(() => validateRuntimeRevision(labels, source));
  for (const imageLabels of [
    undefined,
    {},
    { ...labels, "gents.eval.source-dirty": "true" },
    { ...labels, "org.opencontainers.image.revision": "b".repeat(40) },
  ])
    assert.throws(() => validateRuntimeRevision(imageLabels, revision));
});

test("host memory preflight budgets every container and VM overhead", () => {
  const gib = 1024 ** 3;
  assert.equal(hostMemoryPlan(16 * gib - 1, 30).sufficient, false);
  assert.equal(hostMemoryPlan(24 * gib, 30).sufficient, true);
  assert.equal(hostMemoryPlan(2 * gib, 1).sufficient, true);
  for (const concurrency of [0, 31, 1.5, NaN])
    assert.throws(() => hostMemoryPlan(24 * gib, concurrency));
  assert.throws(() => hostMemoryPlan(NaN, 1));
});

test("live hosts reject mutable or missing image references before starting", async () => {
  for (const runtimeImage of [
    undefined,
    "gents-eval-runtime:development",
    "sha256:bad",
  ]) {
    await assert.rejects(
      HostEnvironment.start({ runtime: true, runtimeImage }),
      /cohort-pinned image/,
    );
  }
});

test(
  "archives actual tmpfs contents privately without docker cp",
  {
    skip: process.env.GENTS_HOST_FIXTURE_TEST !== "1",
    timeout: 180_000,
  },
  async () => {
    await HostEnvironment.build();
    const host = await HostEnvironment.start();
    const directory = await mkdtemp(join(tmpdir(), "gents-host-archive-"));
    try {
      const archive = await archiveContainerDirectory(
        host.id,
        "/host",
        join(directory, "evidence"),
      );
      const { stdout } = await promisify(execFile)("tar", [
        "-xOf",
        archive,
        "./data/records.txt",
      ]);
      assert.equal(stdout, "Important application data\n");
      assert.equal((await stat(archive)).mode & 0o777, 0o600);
      await assert.rejects(
        archiveContainerDirectory(
          host.id,
          "/host",
          join(directory, "evidence"),
        ),
        /EEXIST/,
      );
    } finally {
      await host.close();
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  "host fixture has real isolated faults and repair effects",
  {
    skip: process.env.GENTS_HOST_FIXTURE_TEST !== "1",
    timeout: 180_000,
  },
  async () => {
    await HostEnvironment.build();
    const host = await HostEnvironment.start();
    let neighbor;
    try {
      neighbor = await HostEnvironment.start();
      const healthy = await host.snapshot();
      assert.equal(healthy.api.exit_code, 0);
      assert.equal(healthy.api.body, "healthy");
      assert.equal(healthy.dashboard, "Dashboard ready");
      await host.inject("api-permission");
      const failed = (await host.snapshot()).api;
      assert.equal(failed.exit_code, 1);
      assert.match(failed.diagnostic, /503 Service Unavailable/);
      assert.equal((await neighbor.snapshot()).api.body, "healthy");
      await host.exec(["chmod", "700", "/host/api-work"]);
      assert.equal((await host.snapshot()).api.body, "healthy");
      await host.inject("stale-backup");
      assert.ok(
        (await host.snapshot()).backup_mtime < Date.now() / 1000 - 86400,
      );
      await host.inject("disk-pressure");
      const occupied = await host.snapshot();
      assert.match(occupied.disk, /\b8[0-9]%/);
      assert.equal(occupied.data_hashes, healthy.data_hashes);
      await assert.rejects(host.inject("unknown"), /Unknown host fault/);
      await assert.rejects(host.exec(["touch", "/opt/forbidden"]));
    } finally {
      await Promise.all([host.close(), neighbor?.close()]);
    }
  },
);
