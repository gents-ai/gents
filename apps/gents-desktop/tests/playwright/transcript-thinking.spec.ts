import { expect, gotoHarness, test } from "./desktopTest";

/* While a reply streams at the foot, the transcript only ever moves down
   with it. The Thinking line's spinner swaps a glyph every 100 ms; in
   WebKit each swap laid the page out again and stepped the view back 2 px,
   so everything above the line shook. */
test("following a reply, the view never steps back while the spinner turns", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await expect(page.getByTestId("activity-status")).toBeVisible();
  await page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="activity-status"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const tops: number[] = [];
    scroller.addEventListener("scroll", () => tops.push(scroller.scrollTop));
    Object.assign(window, { __scrollTops: tops });
  });
  for (let n = 0; n < 20; n += 1) {
    await page.evaluate(
      (n) =>
        window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
          ` word${n} and enough more words that the reply wraps onto new lines,`,
        ),
      n,
    );
    await page.waitForTimeout(120);
  }
  const tops = await page.evaluate(
    () => (window as unknown as { __scrollTops: number[] }).__scrollTops,
  );
  const backward = tops.filter((top, i) => i > 0 && top < tops[i - 1]!);
  expect(tops.length).toBeGreaterThan(5);
  expect(backward).toEqual([]);
});
