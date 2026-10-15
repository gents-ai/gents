import { describe, expect, it, vi } from "vitest";

import { newestWins, newestWinsBy, singleFlight } from "../src/lib/reads";

/* a promise the test settles */
function later<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe("newestWins", () => {
  it("keeps only the newest read current", () => {
    const reads = newestWins();
    const first = reads.begin();
    const second = reads.begin();
    expect(first()).toBe(false);
    expect(second()).toBe(true);
  });

  it("outdates every read still out on supersede", () => {
    const reads = newestWins();
    const out = reads.begin();
    reads.supersede();
    expect(out()).toBe(false);
    expect(reads.begin()()).toBe(true);
  });

  it("keeps each key's reads apart", () => {
    const reads = newestWinsBy<string>();
    const a = reads.begin("a");
    const b = reads.begin("b");
    reads.begin("a");
    expect(a()).toBe(false);
    expect(b()).toBe(true);
    reads.supersede("b");
    expect(b()).toBe(false);
  });
});

describe("singleFlight", () => {
  it("joins a call made while a run is under way, and starts afresh after", async () => {
    const answer = later<string>();
    const run = vi.fn(() => answer.promise);
    const flight = singleFlight(run);

    const first = flight();
    const second = flight();
    expect(second).toBe(first);
    expect(flight.running).toBe(true);
    expect(run).toHaveBeenCalledTimes(1);

    answer.resolve("done");
    await expect(first).resolves.toBe("done");
    expect(flight.running).toBe(false);

    run.mockResolvedValueOnce("again");
    await expect(flight()).resolves.toBe("again");
    expect(run).toHaveBeenCalledTimes(2);
  });

  it("lets the next call start after a run fails", async () => {
    const answer = later<string>();
    const flight = singleFlight(vi.fn(() => answer.promise));
    const first = flight();
    answer.reject(new Error("down"));
    await expect(first).rejects.toThrow("down");
    expect(flight.running).toBe(false);
  });
});
