import {
  captureStableScreenshot,
  expect,
  gotoHarness,
  openChat,
  openConfig,
  test,
} from "./desktopTest";

test.describe("desktop stable screenshot states", () => {
  test("captures core shell states", async ({ page }, testInfo) => {
    await gotoHarness(page);
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    await captureStableScreenshot(page, testInfo, "stable-fleet-dashboard");

    await openChat(page);
    await expect(page.getByTestId("transcript-panel")).toBeVisible();
    await captureStableScreenshot(page, testInfo, "stable-chat-transcript");

    await page.getByRole("button", { name: /open operations drawer/i }).click();
    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();
    await captureStableScreenshot(page, testInfo, "stable-operations-drawer");

    await gotoHarness(page);
    await openConfig(page);
    await expect(page.locator(".config-workspace")).toBeVisible();
    await captureStableScreenshot(page, testInfo, "stable-config-workspace");

    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("fleet-empty")).toBeVisible();
    await captureStableScreenshot(page, testInfo, "stable-empty-fleet");

    await gotoHarness(page, "bridge-unavailable");
    await expect(page.getByTestId("error-banner")).toBeVisible();
    await captureStableScreenshot(page, testInfo, "stable-bridge-error");
  });
});
