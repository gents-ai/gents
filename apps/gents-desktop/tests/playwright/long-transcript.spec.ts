import { expect, gotoHarness, test } from "./desktopTest";

/* The two places that read the transcript's geometry, in a real layout:
   landing at the foot, and keeping the reader's row in place while an
   older page lands. */
test.describe("a long transcript", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "chromium-desktop", "one layout is enough");
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start", { exact: false })
      .last()
      .waitFor();
  });

  test("opens at its foot", async ({ page }) => {
    const viewport = page
      .getByTestId("transcript-panel")
      .locator("xpath=ancestor::*[@data-slot='scroll-area-viewport'][1]");
    await expect
      .poll(() =>
        viewport.evaluate((v) =>
          Math.round(v.scrollHeight - v.scrollTop - v.clientHeight),
        ),
      )
      .toBeLessThan(4);
    const live = page
      .getByTestId("transcript-panel")
      .getByText("stream-start", { exact: false })
      .last();
    await expect(live).toBeInViewport();
  });

  test("keeps the reader's row in place while an older page lands", async ({
    page,
  }) => {
    /* reaching the top asks for the older page; the row under the reader then
       is the one that must not move */
    const { key, before, rowsBefore } = await page
      .getByTestId("transcript-panel")
      .evaluate((panel) => {
        const scroller = panel.closest(
          '[data-slot="scroll-area-viewport"]',
        ) as HTMLElement;
        const rowsBefore = scroller.querySelectorAll("[data-timeline-key]").length;
        scroller.scrollTop = 0;
        const first = scroller.querySelector<HTMLElement>("[data-timeline-key]")!;
        return {
          key: first.dataset.timelineKey!,
          before: first.getBoundingClientRect().top,
          rowsBefore,
        };
      });
    const row = page.locator(`[data-timeline-key="${key}"]`);
    await expect
      .poll(() => page.locator("[data-timeline-key]").count())
      .toBeGreaterThan(rowsBefore);
    await expect
      .poll(async () => Math.abs((await row.boundingBox())!.y - before))
      .toBeLessThan(4);
  });
});
