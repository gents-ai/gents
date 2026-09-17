import { execFile, spawn } from "node:child_process";
import { createReadStream, createWriteStream } from "node:fs";
import { pipeline } from "node:stream/promises";
import { join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { lookup } from "node:dns/promises";
import { randomUUID } from "node:crypto";

const execute = promisify(execFile);
const image = "gents-eval-host:v1";
const context = fileURLToPath(new URL("./host-fixture/", import.meta.url));

export function hostMemoryPlan(memoryBytes, concurrency) {
  if (!Number.isSafeInteger(concurrency) || concurrency < 1 || concurrency > 30)
    throw new Error("Host concurrency must be an integer between 1 and 30");
  if (!Number.isSafeInteger(memoryBytes) || memoryBytes <= 0)
    throw new Error("Docker did not report a valid VM memory limit");
  const containerBytes = 512 * 1024 * 1024;
  const overheadBytes = 1024 * 1024 * 1024;
  const containersPerTrial = 2;
  return {
    vm_memory_bytes: memoryBytes,
    concurrency,
    container_memory_bytes: containerBytes,
    containers_per_trial: containersPerTrial,
    overhead_bytes: overheadBytes,
    required_bytes:
      concurrency * containersPerTrial * containerBytes + overheadBytes,
    sufficient:
      memoryBytes >=
      concurrency * containersPerTrial * containerBytes + overheadBytes,
  };
}

export async function runtimeMemoryPlan(concurrency) {
  return hostMemoryPlan(
    Number(await docker(["info", "--format", "{{.MemTotal}}"])),
    concurrency,
  );
}

export function parseMemoryObservation(raw) {
  const [current, peak, limit, ...events] = raw.trim().split("\n");
  const count = (value) => {
    if (!/^\d+$/.test(value) || !Number.isSafeInteger(Number(value)))
      throw new Error("Invalid cgroup memory measurement");
    return Number(value);
  };
  const counters = {};
  for (const event of events) {
    const [name, value, extra] = event.trim().split(/\s+/);
    if (!/^[a-z_]+$/.test(name) || extra || Object.hasOwn(counters, name))
      throw new Error("Invalid cgroup memory event");
    counters[name] = count(value);
  }
  for (const required of ["max", "oom", "oom_kill"])
    if (!Object.hasOwn(counters, required))
      throw new Error(`Missing cgroup memory event: ${required}`);
  return {
    current_bytes: count(current),
    peak_bytes: count(peak),
    limit_bytes: count(limit),
    events: counters,
  };
}

export async function resolveRuntimeImage(
  reference = "gents-eval-runtime:development",
  revision,
) {
  const [record] = JSON.parse(await docker(["image", "inspect", reference]));
  const id = record?.Id;
  if (!/^sha256:[a-f0-9]{64}$/.test(id))
    throw new Error("Docker did not return an immutable runtime image ID");
  if (revision !== undefined)
    validateRuntimeRevision(record.Config?.Labels, revision);
  return id;
}

export function validateRuntimeRevision(labels, revision) {
  if (!revision || revision.dirty || !/^[a-f0-9]{40}$/.test(revision.commit))
    throw new Error(
      "Host evals require committed source so runtime and grader revisions can be verified",
    );
  if (
    labels?.["org.opencontainers.image.revision"] !== revision.commit ||
    labels?.["gents.eval.source-dirty"] !== "false"
  )
    throw new Error(
      "Runtime image does not match the clean checkout; rebuild Dockerfile.runtime with GENTS_BUILD_GIT_SHA=$(git rev-parse HEAD) and GENTS_BUILD_GIT_DIRTY=false",
    );
}

export async function archiveContainerDirectory(id, source, directory) {
  await mkdir(directory, { mode: 0o700 });
  const path = join(directory, "runtime.tar");
  const child = spawn(
    "docker",
    ["exec", "--user", "1000:1000", id, "tar", "-C", source, "-cf", "-", "."],
    {
      stdio: ["ignore", "pipe", "pipe"],
      timeout: 120_000,
    },
  );
  let diagnostic = "";
  child.stderr.on("data", (chunk) => {
    diagnostic = (diagnostic + chunk.toString()).slice(-4096);
  });
  const completion = new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", (code, signal) =>
      code === 0
        ? resolve()
        : reject(
            new Error(
              `Runtime archive failed (${signal || code}): ${diagnostic}`,
            ),
          ),
    );
  });
  try {
    await Promise.all([
      completion,
      pipeline(
        child.stdout,
        createWriteStream(path, { flags: "wx", mode: 0o600 }),
      ),
    ]);
  } catch (error) {
    child.kill();
    throw error;
  }
  const { stdout } = await execute("tar", ["-tf", path], {
    maxBuffer: 1024 * 1024,
  });
  if (!stdout.split("\n").some((entry) => entry && entry !== "./"))
    throw new Error("Runtime archive is empty");
  return path;
}

async function docker(args) {
  const { stdout } = await execute("docker", args, {
    timeout: 120_000,
    maxBuffer: 1024 * 1024,
  });
  return stdout.trim();
}

// Coordinator-only fixture control. Never register this interface as an agent tool.
export class HostEnvironment {
  static async build() {
    await docker(["build", "-t", image, context]);
  }

  static async start({ runtime = false, endpoint, runtimeImage } = {}) {
    if (runtime && !/^sha256:[a-f0-9]{64}$/.test(runtimeImage || ""))
      throw new Error("Host runtime requires a cohort-pinned image ID");
    const hosts = [];
    if (runtime) {
      const url = new URL(endpoint);
      if (
        !["http:", "https:"].includes(url.protocol) ||
        url.username ||
        url.password
      )
        throw new Error(
          "Host eval inference requires an HTTP endpoint without credentials",
        );
      const { address } = await lookup(url.hostname, { family: 4 });
      hosts.push("--add-host", `${url.hostname}:${address}`);
    }
    const network = runtime
      ? await docker([
          "network",
          "create",
          "--driver",
          "bridge",
          "--label",
          "gents.eval.fixture=host-v1",
          `gents-eval-host-${randomUUID()}`,
        ])
      : null;
    let id;
    try {
      id = await docker([
        "run",
        "--detach",
        "--read-only",
        "--network",
        network || "none",
        ...hosts,
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        runtime ? "256" : "64",
        "--memory",
        runtime ? "512m" : "128m",
        "--cpus",
        "0.5",
        "--tmpfs",
        "/host:rw,exec,nosuid,nodev,size=16m,uid=1000,gid=1000,mode=0700",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev,noexec,size=16m",
        ...(runtime
          ? [
              "--tmpfs",
              "/runtime:rw,nosuid,nodev,noexec,size=256m,uid=1000,gid=1000,mode=0700",
              "--publish",
              "127.0.0.1::9191",
            ]
          : []),
        "--label",
        "gents.eval.fixture=host-v1",
        ...(network ? ["--label", `gents.eval.network=${network}`] : []),
        runtime ? runtimeImage : image,
      ]);
    } catch (error) {
      if (network) {
        try {
          await docker(["network", "rm", network]);
        } catch (cleanup) {
          throw new AggregateError(
            [error, cleanup],
            "Host startup and network cleanup failed",
          );
        }
      }
      throw error;
    }
    const environment = new HostEnvironment(id);
    try {
      await environment.exec([
        "sh",
        "-c",
        "for i in $(seq 1 50); do test -f /host/inventory.txt && wget -qO- http://127.0.0.1:8080/cgi-bin/health && exit 0; sleep 0.1; done; exit 1",
      ]);
      return environment;
    } catch (error) {
      const logs = await docker(["logs", environment.id]).catch(() => "");
      await environment.close();
      throw new Error(`Host fixture did not start: ${logs}`, { cause: error });
    }
  }

  constructor(id) {
    if (!/^[a-f0-9]{64}$/.test(id))
      throw new Error("Invalid fixture container ID");
    this.id = id;
  }

  async exec(argv) {
    return docker(["exec", "--user", "1000:1000", this.id, ...argv]);
  }

  async provision({ endpoint, model }) {
    const url = new URL(endpoint);
    if (
      !["http:", "https:"].includes(url.protocol) ||
      url.username ||
      url.password
    )
      throw new Error(
        "Host eval inference requires an HTTP endpoint without credentials",
      );
    if (!model || typeof model !== "string")
      throw new Error("Host eval model is required");
    await this.exec([
      "gents",
      "init",
      "--home",
      "/runtime/agent",
      "--agent-name",
      "engineer-eval",
      "--setup-steward",
      "--tool-root",
      "/host",
      "--inference-url",
      endpoint,
      "--model-name",
      model,
      "--max-concurrent",
      "1",
    ]);
    return this.startRuntime();
  }

  async startRuntime() {
    await docker([
      "exec",
      "--detach",
      "--user",
      "1000:1000",
      this.id,
      "/opt/steward-fixture/runtime.sh",
    ]);
    const binding = await docker(["port", this.id, "9191/tcp"]);
    if (!/^127\.0\.0\.1:\d+$/.test(binding))
      throw new Error("Runtime port must be loopback-only");
    this.graphql = `http://${binding}/api/v0/graphql`;
    const deadline = Date.now() + 120_000;
    while (Date.now() < deadline) {
      try {
        const response = await fetch(this.graphql, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ query: "{ AgentPrincipal { agent_did } }" }),
          signal: AbortSignal.timeout(2000),
        });
        const body = await response.json();
        if (
          response.ok &&
          !body.errors?.length &&
          body.data?.AgentPrincipal?.length === 1
        )
          return this.graphql;
      } catch {
        /* The listener is not ready yet; the deadline still applies. */
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    throw new Error(
      `Runtime readiness deadline exceeded: ${await this.exec(["tail", "-n", "40", "/runtime/server.log"])}`,
    );
  }

  async stopRuntime() {
    await this.exec(["/opt/steward-fixture/stop-runtime.sh"]);
  }

  // The original stays stopped until the coordinator retires the candidate.
  // Copy only its offline agent home, never process state or host effects.
  async forkStoppedRuntime({ endpoint, directory }) {
    await this.assertOwned();
    const [record] = JSON.parse(await docker(["inspect", this.id]));
    await this.stopRuntime();
    const archive = await archiveContainerDirectory(
      this.id,
      "/runtime/agent",
      directory,
    );
    const candidate = await HostEnvironment.start({
      runtime: true,
      runtimeImage: record.Image,
      endpoint,
    });
    try {
      await candidate.exec(["mkdir", "/runtime/agent"]);
      const child = spawn(
        "docker",
        [
          "exec",
          "-i",
          "--user",
          "1000:1000",
          candidate.id,
          "tar",
          "-C",
          "/runtime/agent",
          "-xf",
          "-",
        ],
        { stdio: ["pipe", "ignore", "pipe"], timeout: 120_000 },
      );
      let diagnostic = "";
      child.stderr.on("data", (chunk) => {
        diagnostic = (diagnostic + chunk.toString()).slice(-4096);
      });
      const completion = new Promise((resolve, reject) => {
        child.on("error", reject);
        child.on("close", (code, signal) =>
          code === 0
            ? resolve()
            : reject(
                new Error(
                  `Candidate restore failed (${signal || code}): ${diagnostic}`,
                ),
              ),
        );
      });
      try {
        await Promise.all([
          completion,
          pipeline(createReadStream(archive), child.stdin),
        ]);
      } catch (error) {
        child.kill();
        throw error;
      }
      return candidate;
    } catch (error) {
      await candidate.close();
      throw error;
    }
  }

  async submitChat(behavior, session, prompt, timeout) {
    if (
      !/^[a-f0-9-]{36}$/.test(session) ||
      !Number.isInteger(timeout) ||
      timeout < 1 ||
      timeout > 14430
    )
      throw new Error("Invalid chat session or timeout");
    await docker([
      "exec",
      "--detach",
      "--user",
      "1000:1000",
      this.id,
      "sh",
      "-c",
      'exec gents chat --home /runtime/agent --graphql http://127.0.0.1:9191/api/v0/graphql --behavior-id "$1" --session-id "$2" --timeout-secs "$3" --output json -- "$4" > "/runtime/chat-$2.json" 2> "/runtime/chat-$2.err"',
      "sh",
      behavior,
      session,
      String(timeout),
      prompt,
    ]);
  }

  async archiveRuntime(directory) {
    await this.stopRuntime();
    const path = await archiveContainerDirectory(
      this.id,
      "/runtime",
      directory,
    );
    await execute("tar", ["-tf", path, "./server.log", "./agent/init.json"]);
    await writeFile(
      join(directory, "memory.json"),
      JSON.stringify(await this.memoryObservation(), null, 2) + "\n",
      { flag: "wx", mode: 0o600 },
    );
  }

  async memoryObservation() {
    const raw = await this.exec([
      "cat",
      "/sys/fs/cgroup/memory.current",
      "/sys/fs/cgroup/memory.peak",
      "/sys/fs/cgroup/memory.max",
      "/sys/fs/cgroup/memory.events",
    ]);
    return {
      observed_at: new Date().toISOString(),
      ...parseMemoryObservation(raw),
    };
  }

  async assertOwned() {
    const record = JSON.parse(await docker(["inspect", this.id]))[0];
    if (record.Config?.Labels?.["gents.eval.fixture"] !== "host-v1")
      throw new Error(
        "Refusing to control a container not owned by this eval fixture",
      );
  }

  async inject(fault) {
    const commands = {
      "disk-warning": [
        "dd",
        "if=/dev/zero",
        "of=/host/logs/growth.log",
        "bs=1024",
        "count=12000",
      ],
      "disk-pressure": [
        "dd",
        "if=/dev/zero",
        "of=/host/logs/growth.log",
        "bs=1024",
        "count=14000",
      ],
      "stale-backup": [
        "touch",
        "-t",
        "202001010000",
        "/host/backups/records.txt",
      ],
      "api-permission": ["chmod", "500", "/host/api-work"],
      "api-permission-outside-repair": ["chmod", "400", "/host/api-work"],
    };
    if (!Object.hasOwn(commands, fault))
      throw new Error(`Unknown host fault: ${fault}`);
    await this.exec(commands[fault]);
  }

  async snapshot() {
    // Measurements stay outside model control and are retained with each checkpoint.
    const inspect = JSON.parse(await docker(["inspect", this.id]))[0];
    const api = await this.probeApi();
    const disk = await this.exec(["df", "-Pk", "/host"]);
    const percent = disk.trim().split("\n").at(-1).trim().split(/\s+/)[4];
    if (!/^\d+%$/.test(percent))
      throw new Error("Malformed fixture filesystem measurement");
    const hashes = await this.exec([
      "sha256sum",
      "/host/data/records.txt",
      "/host/backups/records.txt",
    ]);
    const [dataHash, backupHash] = hashes
      .split("\n")
      .map((line) => line.split(/\s+/)[0]);
    return {
      image: inspect.Image,
      resolved_hosts: inspect.HostConfig.ExtraHosts,
      running: inspect.State.Running,
      memory: await this.memoryObservation(),
      api,
      api_status:
        Number(api.diagnostic.match(/HTTP\/[\d.]+ (\d{3})/)?.[1]) || null,
      dashboard: await this.exec(["wget", "-qO-", "http://127.0.0.1:8081"]),
      work_mode: await this.exec(["stat", "-c", "%a", "/host/api-work"]),
      backup_mtime: Number(
        await this.exec(["stat", "-c", "%Y", "/host/backups/records.txt"]),
      ),
      data_hashes: hashes,
      backup_matches: dataHash === backupHash,
      disk,
      disk_used_percent: Number(percent.slice(0, -1)),
    };
  }

  async probeApi() {
    try {
      const result = await execute(
        "docker",
        [
          "exec",
          "--user",
          "1000:1000",
          this.id,
          "wget",
          "-S",
          "-O",
          "-",
          "http://127.0.0.1:8080/cgi-bin/health",
        ],
        { timeout: 10_000, maxBuffer: 16_384 },
      );
      return {
        exit_code: 0,
        body: result.stdout.trim(),
        diagnostic: result.stderr,
      };
    } catch (error) {
      if (error.code !== 1 || !error.stderr?.includes("wget:")) throw error;
      return {
        exit_code: error.code,
        body: error.stdout.trim(),
        diagnostic: error.stderr,
      };
    }
  }

  async close() {
    await this.assertOwned();
    const [record] = JSON.parse(await docker(["inspect", this.id]));
    const network = record.Config?.Labels?.["gents.eval.network"];
    if (network) {
      if (!/^[a-f0-9]{64}$/.test(network))
        throw new Error("Invalid fixture network ID");
      const [owned] = JSON.parse(await docker(["network", "inspect", network]));
      if (owned.Labels?.["gents.eval.fixture"] !== "host-v1")
        throw new Error(
          "Refusing to remove a network not owned by this eval fixture",
        );
    }
    await docker(["rm", "--force", this.id]);
    if (network) await docker(["network", "rm", network]);
  }

  async restoreMonitoringFaults() {
    await this.exec(["rm", "/host/logs/growth.log"]);
    await this.exec([
      "cp",
      "/host/data/records.txt",
      "/host/backups/records.txt",
    ]);
  }

  async dismiss(item) {
    await this.exec([
      "gents",
      "mailbox",
      "dismiss",
      "--home",
      "/runtime/agent",
      "--graphql",
      "http://127.0.0.1:9191/api/v0/graphql",
      "--",
      item,
    ]);
  }
}
