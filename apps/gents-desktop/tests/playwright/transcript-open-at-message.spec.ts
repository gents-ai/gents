import { expect, test } from "./desktopTest";

test("the context meter opens a compaction where it happened, far before the newest page", async ({
  page,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "chromium-narrow",
    "the meter's list is in the header",
  );
  await page.setViewportSize({ width: 1400, height: 900 });
  await page.goto(
    "/tests/ui-harness/harness.html?scenario=mobile-performance&compactionAt=41",
  );
  await page.getByTestId("session-session-large").click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  const marker = page.locator('[data-compaction-key="large-compaction"]');
  /* the newest page doesn't reach back to message 41 */
  await expect(marker).toHaveCount(0);

  /* the header's compact meter: the transcript's own has scrolled away */
  await page.getByTestId("context-meter-compact").click();
  await page
    .getByRole("list", { name: "Recent session compactions" })
    .getByRole("button", { name: /90k → 3k/ })
    .click();

  await expect(marker).toBeInViewport();
  /* the meter's details close behind it */
  await expect(
    page.getByRole("list", { name: "Recent session compactions" }),
  ).toHaveCount(0);
  const summary = page.getByRole("region", { name: "Compaction summary" });
  await expect(summary).toContainText("Keep the large fixture going.");
  await expect(marker.getByRole("button", { name: /Compacted/ })).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  /* the messages around it are drawn, not the history before them */
  await expect(panel.getByText("User fixture row 40:")).toBeVisible();
  await expect(panel.getByText("User fixture row 2:", { exact: false })).toHaveCount(0);
});

test("a compaction whose marker isn't drawn lands on its message and settles", async ({
  page,
}, testInfo) => {
  test.skip(
    testInfo.project.name === "chromium-narrow",
    "the meter's list is in the header",
  );
  await page.setViewportSize({ width: 1400, height: 900 });
  await page.goto(
    "/tests/ui-harness/harness.html?scenario=mobile-performance&compactionAt=41&compactionUnmarked=1",
  );
  await page.getByTestId("session-session-large").click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await page.getByTestId("context-meter-compact").click();
  await page
    .getByRole("list", { name: "Recent session compactions" })
    .getByRole("button", { name: /90k → 3k/ })
    .click();
  /* the page around message 41 is read and the reader lands there, with
     nothing left waiting to open */
  await expect(panel.getByText("Assistant fixture row 41:")).toBeInViewport();
  await expect(page.getByRole("region", { name: "Compaction summary" })).toHaveCount(0);
  /* settled: asking again goes there again */
  await panel.hover();
  await page.mouse.wheel(0, 4000);
  await page.getByTestId("context-meter-compact").click();
  await page
    .getByRole("list", { name: "Recent session compactions" })
    .getByRole("button", { name: /90k → 3k/ })
    .click();
  await expect(panel.getByText("Assistant fixture row 41:")).toBeInViewport();
});
