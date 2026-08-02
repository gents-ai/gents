import { EventEmitter } from "node:events";

import { describe, expect, it } from "vitest";

import { runWithWatchdogRetry, stopProcess } from "./runner-control.mjs";

class FakeChild extends EventEmitter {
  exitCode: number | null = null;
  signalCode: string | null = null;
  readonly killSignals: string[] = [];

  constructor(readonly pid: number) {
    super();
  }

  kill(signal: string) {
    this.killSignals.push(signal);
    this.finish(null, signal);
    return true;
  }

  finish(code: number | null, signal: string | null = null) {
    if (this.exitCode !== null || this.signalCode !== null) {
      return;
    }
    this.exitCode = code;
    this.signalCode = signal;
    this.emit("exit", code, signal);
  }
}

describe("Bombadil watchdog recovery", () => {
  it("retries once with a fresh process only after a watchdog timeout", async () => {
    const children: FakeChild[] = [];
    const stopped: number[] = [];
    const result = await runWithWatchdogRetry({
      timeoutMs: 5,
      startAttempt: (attempt: number) => {
        const child = new FakeChild(100 + attempt);
        children.push(child);
        if (attempt === 2) {
          queueMicrotask(() => child.finish(0));
        }
        return child;
      },
      stopTimedOutChild: (child: FakeChild) => {
        stopped.push(child.pid);
        child.finish(null, "SIGKILL");
      },
    });

    expect(result).toEqual({ kind: "exit", code: 0, attempts: 2 });
    expect(children).toHaveLength(2);
    expect(stopped).toEqual([101]);
  });

  it("does not retry an ordinary Bombadil failure", async () => {
    let attempts = 0;
    const result = await runWithWatchdogRetry({
      timeoutMs: 50,
      startAttempt: () => {
        attempts += 1;
        const child = new FakeChild(200 + attempts);
        queueMicrotask(() => child.finish(7));
        return child;
      },
    });

    expect(result).toEqual({ kind: "exit", code: 7, attempts: 1 });
    expect(attempts).toBe(1);
  });

  it("fails after the single bounded watchdog retry is exhausted", async () => {
    let attempts = 0;
    let stopped = 0;
    const result = await runWithWatchdogRetry({
      timeoutMs: 2,
      startAttempt: () => {
        attempts += 1;
        return new FakeChild(250 + attempts);
      },
      stopTimedOutChild: (child: FakeChild) => {
        stopped += 1;
        child.finish(null, "SIGKILL");
      },
    });

    expect(result).toEqual({ kind: "timeout", attempts: 2 });
    expect(attempts).toBe(2);
    expect(stopped).toBe(2);
  });

  it("terminates the complete Bombadil process group", async () => {
    const child = new FakeChild(321);
    const signals: Array<[number, string]> = [];

    await stopProcess(child, {
      killProcessGroup: true,
      graceMs: 1,
      killByPid: (pid: number, signal: string | number) => {
        if (signal === 0) {
          const error = new Error("process group drained") as NodeJS.ErrnoException;
          error.code = "ESRCH";
          throw error;
        }
        const namedSignal = String(signal);
        signals.push([pid, namedSignal]);
        if (namedSignal === "SIGTERM") {
          child.finish(null, namedSignal);
        }
        return true;
      },
    });

    expect(signals).toEqual([
      [-321, "SIGTERM"],
      [-321, "SIGKILL"],
    ]);
    expect(child.killSignals).toEqual([]);
  });

  it("preserves a successful run when macOS denies cleanup of its exited group", async () => {
    const child = new FakeChild(432);
    child.finish(0);
    const signals: Array<[number, string]> = [];

    await stopProcess(child, {
      killProcessGroup: true,
      killByPid: (pid: number, signal: string | number) => {
        signals.push([pid, String(signal)]);
        const error = new Error("operation not permitted") as NodeJS.ErrnoException;
        error.code = "EPERM";
        throw error;
      },
    });

    expect(signals).toEqual([[-432, "SIGTERM"]]);
    expect(child.exitCode).toBe(0);
  });

  it("rejects permission errors while the Bombadil leader is still running", async () => {
    const child = new FakeChild(543);

    await expect(
      stopProcess(child, {
        killProcessGroup: true,
        killByPid: () => {
          const error = new Error("operation not permitted") as NodeJS.ErrnoException;
          error.code = "EPERM";
          throw error;
        },
      }),
    ).rejects.toMatchObject({ code: "EPERM" });
  });

  it("drains the killed Bombadil leader before allowing a retry", async () => {
    const child = new FakeChild(654);
    const signals: Array<[number, string]> = [];
    let exitedAfterKill = false;
    let groupAlive = true;

    await stopProcess(child, {
      killProcessGroup: true,
      graceMs: 1,
      killDrainMs: 100,
      killByPid: (pid: number, signal: string | number) => {
        if (signal === 0) {
          if (groupAlive) return true;
          const error = new Error("process group drained") as NodeJS.ErrnoException;
          error.code = "ESRCH";
          throw error;
        }
        const namedSignal = String(signal);
        signals.push([pid, namedSignal]);
        if (namedSignal === "SIGKILL") {
          setTimeout(() => {
            exitedAfterKill = true;
            groupAlive = false;
            child.finish(null, namedSignal);
          }, 10);
        }
        return true;
      },
    });

    expect(exitedAfterKill).toBe(true);
    expect(signals).toEqual([
      [-654, "SIGTERM"],
      [-654, "SIGKILL"],
    ]);
  });

  it("gives surviving group members the full SIGTERM grace after a clean exit", async () => {
    const child = new FakeChild(865);
    child.finish(0);
    const signals: Array<[number, string]> = [];
    let polls = 0;

    await stopProcess(child, {
      killProcessGroup: true,
      graceMs: 100,
      killByPid: (pid: number, signal: string | number) => {
        if (signal === 0) {
          polls += 1;
          if (polls < 3) return true;
          const error = new Error("process group drained") as NodeJS.ErrnoException;
          error.code = "ESRCH";
          throw error;
        }
        signals.push([pid, String(signal)]);
        return true;
      },
    });

    expect(signals).toEqual([[-865, "SIGTERM"]]);
    expect(polls).toBeGreaterThanOrEqual(3);
  });

  it("keeps a finished run authoritative when its group never drains", async () => {
    const child = new FakeChild(976);
    child.finish(0);
    const signals: Array<[number, string]> = [];

    await expect(
      stopProcess(child, {
        killProcessGroup: true,
        graceMs: 1,
        killDrainMs: 2,
        killByPid: (pid: number, signal: string | number) => {
          if (signal === 0) return true;
          signals.push([pid, String(signal)]);
          return true;
        },
      }),
    ).resolves.toBeUndefined();

    expect(signals).toEqual([
      [-976, "SIGTERM"],
      [-976, "SIGKILL"],
    ]);
    expect(child.exitCode).toBe(0);
  });

  it("does not start over while a SIGKILLed process remains undrained", async () => {
    const child = new FakeChild(777);

    await expect(
      stopProcess(child, {
        killProcessGroup: true,
        graceMs: 1,
        killDrainMs: 2,
        killByPid: () => true,
      }),
    ).rejects.toThrow("did not drain");
  });
});
