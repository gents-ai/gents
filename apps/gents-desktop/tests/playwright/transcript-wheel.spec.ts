import { expect, gotoHarness, test } from "./desktopTest";

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

test("a command's long output scrolls inside its own box", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(3, 0, 60));
  const group = panel.getByRole("button", { name: /3 commands/ });
  await group.scrollIntoViewIfNeeded();
  if ((await group.getAttribute("aria-expanded")) !== "true") await group.click();
  await panel.getByTitle("$ cargo test -p gents step_0").click();
  const output = panel.getByText("output line 59");
  await output.waitFor();
  const box = output.locator("xpath=ancestor::*[@data-slot='scroll-area-viewport'][1]");
  await expect
    .poll(() => box.evaluate((el) => el.scrollHeight > el.clientHeight + 100))
    .toBe(true);
  const rect = (await box.boundingBox())!;
  await page.mouse.move(rect.x + rect.width / 2, rect.y + 20);
  await page.mouse.wheel(0, 200);
  await expect.poll(() => box.evaluate((el) => el.scrollTop)).toBeGreaterThan(100);
});
