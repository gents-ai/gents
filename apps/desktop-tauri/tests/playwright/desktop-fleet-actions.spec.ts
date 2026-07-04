import { expect, gotoHarness, PEER_ID, test } from "./desktopTest";

// The fleet-row navigation is exercised elsewhere via the agent NAME
// (`fleet-chat-name-*`). These tests click the per-row ACTION buttons
// directly, so a regression that breaks the buttons (but not the name) cannot
// ship silently — the class of defect behind "the row buttons do nothing".
test.describe("fleet row action buttons", () => {
  test("chat action button opens the chat workspace", async ({ page }) => {
    await gotoHarness(page, "default");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    const chatAction = page.getByTestId(`fleet-chat-${PEER_ID}`);
    await expect(chatAction).toBeEnabled();
    await chatAction.click();

    await expect(page.getByTestId("composer-input")).toBeVisible();
  });

  test("config action button opens the config workspace", async ({ page }) => {
    await gotoHarness(page, "default");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    const configAction = page.getByTestId(`fleet-config-${PEER_ID}`);
    await expect(configAction).toBeEnabled();
    await configAction.click();

    await expect(page.locator(".config-workspace")).toBeVisible();
  });

  test("P2P repair is fleet-level and hidden while healthy, never a row action", async ({
    page,
  }) => {
    await gotoHarness(page, "default");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    // Repair re-dials the desktop client's connections as a whole, so it must
    // not masquerade as a per-agent row action.
    await expect(page.getByTestId(`fleet-repair-${PEER_ID}`)).toHaveCount(0);
    // The default fixture dials successfully with no last error, so the
    // fleet-level reconnect control is intentionally absent.
    await expect(page.getByTestId("fleet-repair-p2p")).toHaveCount(0);
  });
});
