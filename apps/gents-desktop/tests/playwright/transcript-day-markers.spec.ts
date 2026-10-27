import { expect, gotoHarness, test } from "./desktopTest";

test("marks each day of a session that runs across days", async ({ page }) => {
  await gotoHarness(page, "days");
  await page.getByTestId("session-session-intro").click();
  const panel = page.getByTestId("transcript-panel");
  await expect(panel.getByTestId("day-marker")).toHaveText([
    /^\w+day, .+$/,
    "Yesterday",
    "Today",
  ]);
  const today = panel
    .locator("[data-timeline-key]")
    .filter({ has: page.getByTestId("day-marker").filter({ hasText: /^Today$/ }) });
  await expect(today).toContainText("Good morning. Let's fix it and add a test.");
});
