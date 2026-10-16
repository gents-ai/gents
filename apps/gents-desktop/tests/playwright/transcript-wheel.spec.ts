import { expect, gotoHarness, test } from "./desktopTest";

/* A mouse wheel sends each notch as its own scroll, so wheeling up a
   transcript past an open activity box hands notches to the box. Once the
   box is at its top, the rest must reach the transcript, not stop under the
   pointer. */
test("wheeling up past an open activity box keeps the transcript moving", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(24));
  const group = panel.getByRole("button", { name: /24 commands/ });
  await group.scrollIntoViewIfNeeded();
  await group.click();
  await expect(group).toHaveAttribute("aria-expanded", "true");

  const scroller = panel.locator(
    "xpath=ancestor::*[@data-slot='scroll-area-viewport'][1]",
  );
  /* the box at its own top, under the pointer */
  const box = group.locator("xpath=following-sibling::*[1]");
  await box.evaluate((el) => {
    const viewport = el.querySelector<HTMLElement>("[data-slot=scroll-area-viewport]");
    if (viewport) viewport.scrollTop = 0;
  });
  const rect = (await box.boundingBox())!;
  await page.mouse.move(rect.x + rect.width / 2, rect.y + 20);

  const before = await scroller.evaluate((el) => el.scrollTop);
  for (let notch = 0; notch < 6; notch += 1) {
    await page.mouse.wheel(0, -100);
    await page.waitForTimeout(50);
  }
  await expect
    .poll(() => scroller.evaluate((el) => el.scrollTop))
    .toBeLessThan(before - 200);
});

/* A failure folded into a group would be the row a reader wants and the
   one they could not see: the header counts it, and the group opens. */
test("a group with a failed command says so and starts open", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(6, 2));
  const group = panel.getByRole("button", { name: /6 commands/ });
  await group.scrollIntoViewIfNeeded();
  await expect(group).toHaveText(/Ran 6 commands, 2 failed/);
  await expect(group).toHaveAttribute("aria-expanded", "true");
  await expect(group).toHaveAttribute("data-failed", "true");
});
