import { expect, gotoHarness, test } from "./desktopTest";

test.describe("transcript input at its scroll boundary", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await page.setViewportSize({ width: 390, height: 844 });
    await gotoHarness(page, "mobile-performance");
    await page.getByTestId("session-session-large").click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start")
      .last()
      .waitFor();
  });

  for (const input of ["wheel", "key"] as const) {
    test(`${input} at the top does not retain reader intent`, async ({ page }) => {
      const viewport = page.locator(
        '[data-slot="scroll-area-viewport"][data-following]',
      );
      await viewport.evaluate((scroller, input) => {
        /* Put the view at the boundary before its pending scroll notification
           is delivered, then send input that cannot move it farther. */
        scroller.scrollTop = 0;
        if (input === "wheel")
          scroller.dispatchEvent(
            new WheelEvent("wheel", { deltaY: -40, bubbles: true }),
          );
        else
          window.dispatchEvent(
            new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }),
          );
      }, input);

      /* A later scroll not caused by the reader must still be reconciled to
         the captured row after the no-op input has gone quiet. */
      const before = await viewport.evaluate(async (scroller) => {
        await new Promise((resolve) => setTimeout(resolve, 400));
        const before = scroller.scrollTop;
        scroller.scrollTop = before + 24;
        scroller.dispatchEvent(new Event("scroll"));
        return before;
      });
      await expect
        .poll(() =>
          viewport.evaluate(
            (scroller, captured) => Math.abs(scroller.scrollTop - captured),
            before,
          ),
        )
        .toBeLessThanOrEqual(1);
    });
  }
});
