import { expect, gotoHarness, openChat, openChatNavigation, test } from "./desktopTest";

test.describe("mobile session sync fixture", () => {
  test("opens a pre-existing session and hydrates history without blocking local rows", async ({
    page,
  }) => {
    await gotoHarness(page, "session-hydration");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "syncing",
    );
    await openChat(page);
    await openChatNavigation(page);
    await page.getByTestId("conversation-session-remote").click();
    await expect(
      page.getByTestId("transcript-panel").getByText("hello from desktop"),
    ).toBeVisible();
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "requested",
    );

    await page.evaluate(() => window.__GENTS_SESSION_SYNC__?.progress(2, 4));
    await expect(page.getByTestId("session-hydration-status")).toHaveText(/2 of 4/);
    await expect(
      page
        .getByTestId("transcript-panel")
        .getByText("history arrived from the desktop"),
    ).toBeVisible();

    await page.evaluate(() => window.__GENTS_SESSION_SYNC__?.complete());
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "complete",
    );
    await expect(page.getByTestId("session-hydration-status")).toContainText("4 of 4");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "healthy",
    );
  });

  test("failed hydration retries through the existing session snapshot path", async ({
    page,
  }) => {
    await gotoHarness(page, "session-hydration");
    await openChat(page);
    await openChatNavigation(page);
    await page.getByTestId("conversation-session-remote").click();
    await page.evaluate(() => window.__GENTS_SESSION_SYNC__?.fail());
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "failed",
    );
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "failed",
    );
    await page.getByTestId("session-hydration-retry").click();
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "requested",
    );
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "syncing",
    );
  });

  test("global indicator distinguishes offline, stalled, and failed", async ({
    page,
  }) => {
    await gotoHarness(page, "sync-offline");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "offline",
    );
    await expect(page.getByTestId("sync-health-summary")).toContainText(
      "Offline since",
    );

    await gotoHarness(page, "sync-stalled");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "stalled",
    );
    await page.getByTestId("sync-health-summary").click();
    await expect(page.getByTestId("sync-health-details")).toContainText("RpcTimeout");

    await gotoHarness(page, "sync-failed");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "failed",
    );
  });

  test("offline recovers through the existing reconnect control", async ({ page }) => {
    await gotoHarness(page, "sync-offline");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "offline",
    );
    await page.getByTestId("fleet-repair-p2p").click();
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "healthy",
    );
  });
});
