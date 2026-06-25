import {
  expect,
  gotoHarness,
  openChat,
  openConfig,
  test,
} from "../playwright/desktopTest";

test.describe("desktop visual baselines", () => {
  test("matches stable shell states", async ({ page }) => {
    await gotoHarness(page);
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    await expect(page).toHaveScreenshot("fleet-dashboard.png", {
      animations: "disabled",
      fullPage: true,
    });

    await openChat(page);
    await expect(page.getByTestId("transcript-panel")).toBeVisible();
    await expect(page).toHaveScreenshot("chat-transcript.png", {
      animations: "disabled",
      fullPage: true,
    });

    await page.getByRole("button", { name: /open operations drawer/i }).click();
    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();
    await expect(page).toHaveScreenshot("operations-drawer.png", {
      animations: "disabled",
      fullPage: true,
    });

    await gotoHarness(page);
    await openConfig(page);
    await expect(page.locator(".config-workspace")).toBeVisible();
    await expect(page).toHaveScreenshot("config-workspace.png", {
      animations: "disabled",
      fullPage: true,
    });

    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("fleet-empty")).toBeVisible();
    await expect(page).toHaveScreenshot("empty-fleet.png", {
      animations: "disabled",
      fullPage: true,
    });

    await gotoHarness(page, "bridge-unavailable");
    await expect(page.getByTestId("error-banner")).toBeVisible();
    await expect(page).toHaveScreenshot("bridge-error.png", {
      animations: "disabled",
      fullPage: true,
    });
  });
});
