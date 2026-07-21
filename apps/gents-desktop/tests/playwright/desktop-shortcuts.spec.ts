import { expect, gotoHarness, test } from "./desktopTest";

test.describe("keyboard shortcuts", () => {
  test("switch views, open help, and focus the composer", async ({ page }) => {
    await gotoHarness(page);

    await page.keyboard.press("ControlOrMeta+2");
    await expect(page.getByTestId("composer-input")).toBeVisible();

    await page.keyboard.press("ControlOrMeta+k");
    await expect(page.getByTestId("composer-input")).toBeFocused();

    await page.keyboard.press("ControlOrMeta+1");
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    await page.keyboard.press("ControlOrMeta+/");
    await expect(page.getByTestId("shortcuts-help")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("shortcuts-help")).toHaveCount(0);
  });
});
