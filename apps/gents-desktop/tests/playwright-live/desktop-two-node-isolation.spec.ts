import type { BrowserContext } from "@playwright/test";
import { LiveBridgeRunner } from "../live-bridge-runner";
import { startTwoIndependentNodes } from "../live-bridge-runner/two-node";
import {
  composer,
  expect,
  gotoLiveHarness,
  openChat,
  test,
} from "../playwright/desktopTest";

/* This establishes separate live clients and nodes. Sharing and invitations
   must be exercised by their own journey once those controls are available. */
test("opens two independent live nodes in separate browser contexts", async ({
  browser,
}, testInfo) => {
  const nodes = await startTwoIndependentNodes(() => LiveBridgeRunner.start());
  const contexts: BrowserContext[] = [];
  const errors: string[] = [];
  try {
    const firstContext = await browser.newContext({
      baseURL: testInfo.project.use.baseURL,
    });
    contexts.push(firstContext);
    const secondContext = await browser.newContext({
      baseURL: testInfo.project.use.baseURL,
    });
    contexts.push(secondContext);
    const [first, second] = await Promise.all(
      contexts.map((context) => context.newPage()),
    );
    for (const page of [first, second])
      page.on("pageerror", (error) => errors.push(error.message));

    await Promise.all([
      gotoLiveHarness(first, nodes.first.baseUrl),
      gotoLiveHarness(second, nodes.second.baseUrl),
    ]);
    await Promise.all([openChat(first), openChat(second)]);
    const [firstSnapshot, secondSnapshot] = await Promise.all([
      nodes.first.fetchSnapshot(),
      nodes.second.fetchSnapshot(),
    ]);
    expect(
      firstSnapshot.client?.deployments.some(
        (node) => node.nodeDid === nodes.first.nodeDid,
      ),
    ).toBe(true);
    expect(
      secondSnapshot.client?.deployments.some(
        (node) => node.nodeDid === nodes.second.nodeDid,
      ),
    ).toBe(true);
    expect(
      firstSnapshot.client?.deployments.some(
        (node) => node.nodeDid === nodes.second.nodeDid,
      ),
    ).toBe(false);
    expect(
      secondSnapshot.client?.deployments.some(
        (node) => node.nodeDid === nodes.first.nodeDid,
      ),
    ).toBe(false);
    await composer(first).fill("Draft on the first node");
    await expect(composer(second)).toHaveText("");
    await composer(second).fill("Draft on the second node");
    await expect(composer(first)).toHaveText("Draft on the first node");
    expect(errors).toEqual([]);
  } finally {
    const closed = await Promise.allSettled(contexts.map((context) => context.close()));
    await nodes.dispose();
    const errors = closed.flatMap((result) =>
      result.status === "rejected" ? [result.reason] : [],
    );
    if (errors.length)
      throw new AggregateError(errors, "Could not close both browser contexts.");
  }
});
