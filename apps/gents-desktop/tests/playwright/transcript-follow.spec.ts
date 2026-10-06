import { expect, gotoHarness, test } from "./desktopTest";

/* Scrolled up, the reader's row stays where it is on screen whatever
   changes around it. */
test.describe("a reader scrolled up in a transcript", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start")
      .last()
      .waitFor();
  });

  test("keeps their row still when a row above it grows", async ({ page }) => {
    /* the reader's own wheel takes them off the foot */
    await page.getByTestId("transcript-panel").hover();
    await page.mouse.wheel(0, -1200);
    await page.waitForTimeout(300);
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "false");

    const moved = await viewport.evaluate(async (scroller) => {
      const line = scroller.getBoundingClientRect().top + 200;
      const rows = Array.from(
        scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      );
      const reader = rows.find((row) => row.getBoundingClientRect().bottom > line)!;
      const above = rows[rows.indexOf(reader) - 2]!;
      const before = reader.getBoundingClientRect().top;
      /* something above the reader grows, as a step's output would */
      above.style.paddingTop = "300px";
      await new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      );
      return Math.round(reader.getBoundingClientRect().top - before);
    });
    expect(moved).toBe(0);
  });

  test("keeps their place while the reply below them streams", async ({ page }) => {
    await page.getByTestId("transcript-panel").hover();
    await page.mouse.wheel(0, -1200);
    await page.waitForTimeout(300);
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    const before = await viewport.evaluate((scroller) => scroller.scrollTop);
    for (let n = 0; n < 8; n += 1) {
      await page.evaluate(
        (n) =>
          window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
            `\\n\\nMore of the reply, paragraph ${n}, long enough to wrap onto a second line in the column.`,
          ),
        n,
      );
      await page.waitForTimeout(100);
    }
    await expect(viewport).toHaveAttribute("data-following", "false");
    expect(await viewport.evaluate((scroller) => scroller.scrollTop)).toBe(before);
  });
});

/* Following at the foot, the view stays there when the message the person
   sent is replaced by its saved copy. */
test("stays at the foot when a sent message is saved", async ({ page }, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn("pending"));
  await expect(page.getByTestId("transcript-panel").getByText("again")).toBeVisible();
  await page.waitForTimeout(300);
  const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
  await viewport.evaluate((scroller) => {
    const gaps: number[] = [];
    const tick = () => {
      gaps.push(
        Math.round(scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight),
      );
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __footGaps: gaps });
  });
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn("saved"));
  await page.waitForTimeout(600);
  const gaps = await page.evaluate(
    () => (window as unknown as { __footGaps: number[] }).__footGaps,
  );
  expect([...new Set(gaps)].filter((gap) => gap > 1)).toEqual([]);
});
