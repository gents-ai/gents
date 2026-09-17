import assert from "node:assert/strict";
import test from "node:test";
import {
  HostEnvironment,
  archiveContainerDirectory,
  hostMemoryPlan,
  validateRuntimeRevision,
  resolveRuntimeImage,
  parseMemoryObservation,
} from "./host-environment.mjs";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { control } from "./host-control.mjs";

test(
  "candidate runtime forks an offline home without changing the original host",
  { skip: process.env.GENTS_HOST_FIXTURE_TEST !== "1", timeout: 180_000 },
  async () => {
    const directory = await mkdtemp(join(tmpdir(), "gents-candidate-"));
    const endpoint = "http://127.0.0.1:8000/v1";
    let original;
    let candidate;
    const principal = async (graphql) => {
      const response = await fetch(graphql, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query: "{ AgentPrincipal { agent_did } }" }),
        signal: AbortSignal.timeout(5000),
      });
      const body = await response.json();
      assert.equal(response.ok, true);
      assert.equal(body.errors, undefined);
      assert.equal(body.data.AgentPrincipal.length, 1);
      return body.data.AgentPrincipal[0].agent_did;
    };
    try {
      original = await HostEnvironment.start({
        runtime: true,
        runtimeImage: await resolveRuntimeImage(),
        endpoint,
      });
      const graphql = await original.provision({
        endpoint,
        model: "fixture-no-inference",
      });
      const did = await principal(graphql);
      await original.inject("api-permission");
      const fork = await control(
        ["fork", original.id, join(directory, "snapshot")],
        { GENTS_D4F_ENDPOINT: endpoint },
      );
      candidate = new HostEnvironment(fork.container_id);
      assert.equal(fork.original_container_id, original.id);
      await assert.rejects(
        original.exec(["test", "-e", "/runtime/server.pid"]),
      );
      assert.equal(await principal(fork.graphql), did);
      assert.equal((await candidate.snapshot()).api_status, 200);
      assert.equal((await original.snapshot()).api_status, 503);
      await candidate.exec(["touch", "/host/candidate-only"]);
      await assert.rejects(
        original.exec(["test", "-e", "/host/candidate-only"]),
      );
      await candidate.close();
      candidate = undefined;
      const resumed = await control(["resume", original.id]);
      assert.equal(await principal(resumed.graphql), did);
      assert.equal((await original.snapshot()).api_status, 503);
    } finally {
      await candidate?.close();
      await original?.close();
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  "stopped runtime archives retain private final memory evidence",
  { skip: process.env.GENTS_HOST_FIXTURE_TEST !== "1", timeout: 180_000 },
  async () => {
    const directory = await mkdtemp(join(tmpdir(), "gents-memory-archive-"));
    let host;
    try {
      host = await HostEnvironment.start({
        runtime: true,
        runtimeImage: await resolveRuntimeImage(),
        endpoint: "http://127.0.0.1:8000/v1",
      });
      await host.provision({
        endpoint: "http://127.0.0.1:8000/v1",
        model: "fixture-no-inference",
      });
      const archive = join(directory, "runtime");
      await host.archiveRuntime(archive);
      const path = join(archive, "memory.json");
      const memory = JSON.parse(await readFile(path, "utf8"));
      assert.equal(memory.limit_bytes, 512 * 1024 * 1024);
      assert.ok(memory.peak_bytes > 0);
      assert.equal(memory.events.oom_kill, 0);
      assert.equal((await stat(path)).mode & 0o777, 0o600);
      const receipt = await readFile(path, "utf8");
      await assert.rejects(host.archiveRuntime(archive));
      assert.equal(await readFile(path, "utf8"), receipt);
    } finally {
      await host?.close();
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test("memory evidence preserves limit pressure and OOM counters without coercion", () => {
  const raw = "1024\n2048\n4096\nlow 0\nhigh 0\nmax 2\noom 1\noom_kill 1\n";
  assert.deepEqual(parseMemoryObservation(raw), {
    current_bytes: 1024,
    peak_bytes: 2048,
    limit_bytes: 4096,
    events: { low: 0, high: 0, max: 2, oom: 1, oom_kill: 1 },
  });
  for (const invalid of [
    "",
    raw.replace("4096", "max"),
    raw.replace("oom 1\n", ""),
    `${raw}oom 0\n`,
    raw.replace("1024", "1.5"),
  ])
    assert.throws(() => parseMemoryObservation(invalid));
});

test(
  "live fixture networks prevent direct cross-trial access and are cleaned up",
  {
    skip: process.env.GENTS_HOST_FIXTURE_TEST !== "1",
    timeout: 180_000,
  },
  async () => {
    const execute = promisify(execFile);
    const runtimeImage = await resolveRuntimeImage();
    const hosts = [];
    const networks = [];
    try {
      for (let index = 0; index < 2; index++) {
        const host = await HostEnvironment.start({
          runtime: true,
          runtimeImage,
          endpoint: "http://127.0.0.1:8000/v1",
        });
        hosts.push(host);
        const { stdout } = await execute("docker", ["inspect", host.id]);
        const [record] = JSON.parse(stdout);
        networks.push(record.Config.Labels["gents.eval.network"]);
      }
      assert.notEqual(networks[0], networks[1]);
      const { stdout } = await execute("docker", ["inspect", hosts[1].id]);
      const [record] = JSON.parse(stdout);
      const address = Object.values(record.NetworkSettings.Networks)[0]
        .IPAddress;
      assert.match(address, /^\d+\.\d+\.\d+\.\d+$/);
      assert.equal((await hosts[0].snapshot()).api.body, "healthy");
      await assert.rejects(
        hosts[0].exec([
          "wget",
          "-T",
          "2",
          "-qO-",
          `http://${address}:8080/cgi-bin/health`,
        ]),
      );
    } finally {
      await Promise.all(hosts.map((host) => host.close()));
    }
    for (const network of networks)
      await assert.rejects(execute("docker", ["network", "inspect", network]));
  },
);

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
  assert.equal(hostMemoryPlan(24 * gib, 30).sufficient, false);
  assert.equal(hostMemoryPlan(31 * gib, 30).sufficient, true);
  assert.equal(hostMemoryPlan(31 * gib, 30).containers_per_trial, 2);
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
      assert.equal(healthy.memory.limit_bytes, 128 * 1024 * 1024);
      assert.ok(healthy.memory.peak_bytes > 0);
      assert.equal(healthy.memory.events.oom_kill, 0);
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
      await host.inject("disk-warning");
      const warning = await host.snapshot();
      assert.ok(
        warning.disk_used_percent >= 70 && warning.disk_used_percent < 80,
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
