import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";

import {
  deliveryAffordances,
  type DeliveryAffordances,
  type SendStatus,
} from "../../src/hooks/chatDeliveryView";

type ViewCase = Omit<DeliveryAffordances, "view"> & {
  status: SendStatus;
  mailbox: boolean;
  view: DeliveryAffordances["view"];
};
type Snapshot = {
  status: SendStatus | null;
  payload: boolean;
  turns: number;
  retryOffered: boolean;
};
type Trace = { actions: { kind: string }[]; after: Snapshot[] };
const run = promisify(execFile);

it("derives the send view and actions from the Lean optimistic send owner", async () => {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.ClientOptimisticSend"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "Proofs/Conformance/ClientOptimisticSend.lean"],
    { cwd, timeout: 240_000, maxBuffer: 64 * 1024 * 1024 },
  );
  const { views, traces } = JSON.parse(stdout) as {
    views: ViewCase[];
    traces: Trace[];
  };
  expect(views).toHaveLength(8);
  for (const { status, mailbox, ...expected } of views)
    expect(
      deliveryAffordances(status, mailbox),
      `${status} mailbox=${mailbox}`,
    ).toEqual(expected);
  expect(traces.length).toBeGreaterThan(100);
  for (const trace of traces)
    for (const snapshot of trace.after) {
      // No interleaving makes a second turn, and Retry is offered exactly
      // when the message failed: its payload is always still kept.
      expect(snapshot.turns).toBeLessThanOrEqual(1);
      if (snapshot.status === "pending" || snapshot.status === "failed")
        expect(snapshot.payload).toBe(true);
      expect(snapshot.retryOffered).toBe(
        snapshot.status !== null && deliveryAffordances(snapshot.status, false).retry,
      );
    }
}, 300_000);
