import { expect, gotoHarness, test } from "./desktopTest";

test.describe("transcript input at its scroll boundary", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(testInfo.project.name !== "webkit-desktop", "one layout per engine");
    await page.setViewportSize({ width: 390, height: 844 });
    await gotoHarness(page, "mobile-performance");
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.setHistorySize(20));
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
      /* a correction the opening still carries lands with the row it holds,
         so the jump is made once none is carried: the view then rests at 0 */
      await expect
        .poll(() =>
          viewport.evaluate((scroller) => {
            const content = scroller.querySelector<HTMLElement>(
              "[data-transcript-content]",
            )!;
            if (content.style.marginTop) return false;
            scroller.dispatchEvent(
              new WheelEvent("wheel", { deltaY: -1, bubbles: true }),
            );
            scroller.scrollTop = 0;
            scroller.dispatchEvent(new Event("scroll"));
            return true;
          }),
        )
        .toBe(true);
      await expect
        .poll(() => viewport.evaluate((scroller) => scroller.scrollTop))
        .toBe(0);
      await page.waitForTimeout(400);
      await viewport.evaluate((scroller, input) => {
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

  test("a jump to the top while a correction is carried stays at the top", async ({
    page,
  }) => {
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    /* the reader's wheel keeps the view moving while the reply grows below:
       WebKit drops a write made then, so the owner carries the growth on
       the content, and the reader jumps to the top while it is carried */
    const carried = await viewport.evaluate(async (scroller) => {
      const content = scroller.querySelector<HTMLElement>("[data-transcript-content]")!;
      for (let frame = 0; frame < 120 && !content.style.marginTop; frame += 1) {
        scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: 1, bubbles: true }));
        window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
          "\n\nMore of the reply, long enough to wrap onto a second line in the column.",
        );
        await new Promise((resolve) => requestAnimationFrame(resolve));
      }
      const carried = content.style.marginTop;
      scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -1, bubbles: true }));
      scroller.scrollTop = 0;
      scroller.dispatchEvent(new Event("scroll"));
      return carried;
    });
    expect(carried).not.toBe("");
    await expect
      .poll(() =>
        viewport
          .locator("[data-transcript-content]")
          .evaluate((content: HTMLElement) => content.style.marginTop),
      )
      .toBe("");
    /* landed, the reader still sees the session's first row */
    await expect
      .poll(() =>
        viewport.evaluate((scroller) => {
          const first = scroller.querySelector('[data-row-key][data-index="0"]');
          if (!first) return false;
          return (
            first.getBoundingClientRect().bottom > scroller.getBoundingClientRect().top
          );
        }),
      )
      .toBe(true);
  });
});
