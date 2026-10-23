import { expect, gotoHarness, test } from "./desktopTest";

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
    const rowsBefore = await page.getByTestId("transcript-panel").evaluate((panel) => {
      const scroller = panel.closest(
        '[data-slot="scroll-area-viewport"]',
      ) as HTMLElement;
      scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -1 }));
      scroller.scrollTop = 0;
      return Number(
        panel.querySelector("[data-transcript-list]")!.getAttribute("data-rows"),
      );
    });
    /* the list draws the rows at the new place on its next frames */
    await page.evaluate(
      () =>
        new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        ),
    );
    const { key, before } = await page
      .getByTestId("transcript-panel")
      .evaluate((panel) => {
        const scroller = panel.closest(
          '[data-slot="scroll-area-viewport"]',
        ) as HTMLElement;
        const view = scroller.getBoundingClientRect();
        const first = [
          ...scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
        ].find((row) => row.getBoundingClientRect().bottom > view.top)!;
        return {
          key: first.dataset.timelineKey!,
          before: first.getBoundingClientRect().top,
        };
      });
    const row = page.locator(`[data-timeline-key="${key}"]`);
    await expect
      .poll(() =>
        page
          .locator("[data-transcript-list]")
          .evaluate((list) => Number(list.getAttribute("data-rows"))),
      )
      .toBeGreaterThan(rowsBefore);
    await expect
      .poll(async () => Math.abs((await row.boundingBox())!.y - before))
      .toBeLessThan(4);
  });
});
