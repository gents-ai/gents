import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";

import {
  deliveryAffordances,
  mayResendNewIdentity,
  type DeliveryAffordances,
  type SavedDelivery,
} from "../../src/hooks/chatDeliveryView";

type Case = DeliveryAffordances & {
  saved: SavedDelivery;
  mailbox: boolean;
  mayResendNewIdentity: boolean;
};
const run = promisify(execFile);

it("derives the saved-send view and actions from the Lean receipt owner", async () => {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.ClientDeliveryView"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "Proofs/Conformance/ClientDeliveryView.lean"],
    { cwd, timeout: 240_000 },
  );
  const cases = JSON.parse(stdout) as Case[];
  expect(cases).toHaveLength(12);
  expect(cases.filter((row) => row.setAside).map((row) => row.saved)).toEqual([
    "unknownUnretained",
    "unknownUnretained",
  ]);
  for (const { saved, mailbox, mayResendNewIdentity: resend, ...expected } of cases) {
    const shown = deliveryAffordances(saved, mailbox);
    expect(shown, `${saved} mailbox=${mailbox}`).toEqual(expected);
    expect(mayResendNewIdentity(shown.view)).toBe(resend);
  }
}, 300_000);
