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
      killByPid: (pid: number, signal: string) => {
        signals.push([pid, signal]);
        if (signal === "SIGTERM") {
          child.finish(null, signal);
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
});
