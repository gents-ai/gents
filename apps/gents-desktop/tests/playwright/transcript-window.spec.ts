import { expect, gotoHarness, test } from "./desktopTest";

type Frame = { boxes: number; boxesInView: number; rows: number; height: number };

/* A long transcript draws only the rows near the view; the rest are boxes
   of their own height. A reader scrolling through it never sees a box:
   each row is drawn again while it is still well off screen. */
test("a long transcript shows no undrawn row while the reader scrolls up", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  /* scrolls through every page of the session; WebKit on Linux animates
     each wheel's scroll, so this outlasts the default limit there */
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();

  /* every frame: how many boxes there are, and whether any is in view */
  await page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="transcript-panel"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const seen: Frame[] = [];
    const tick = () => {
      const view = scroller.getBoundingClientRect();
      const boxes = [...scroller.querySelectorAll('[data-window-row="box"]')];
      const boxesInView = boxes.filter((box) => {
        const r = box.getBoundingClientRect();
        return r.bottom > view.top && r.top < view.bottom;
      }).length;
      seen.push({
        boxes: boxes.length,
        boxesInView,
        rows: scroller.querySelectorAll("[data-window-row]").length,
        height: scroller.scrollHeight,
      });
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __windowFrames: seen });
  });

  await panel.hover();
  /* up through every page of the session, as a reader with a wheel */
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(150);
  }
  /* and back down to the foot */
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, 1500);
    await page.waitForTimeout(100);
  }
  await page.waitForTimeout(500);

  const frames = await page.evaluate(
    () => (window as unknown as { __windowFrames: Frame[] }).__windowFrames,
  );
  expect(Math.max(...frames.map((f) => f.boxes))).toBeGreaterThan(50);
  expect(frames.filter((f) => f.boxesInView > 0)).toEqual([]);
  /* a box is its row's height, so swapping rows never changes the page's
     height; only a page of older rows landing does */
  const resized = frames.filter(
    (f, i) =>
      i > 0 && f.rows === frames[i - 1].rows && f.height !== frames[i - 1].height,
  );
  expect(resized).toEqual([]);
});

/* A jump straight to a far part of the session (the scrollbar dragged or
   clicked, Home, End) lands on rows drawn in the same frame, not boxes. */
test("a jump to a far part of a long transcript shows no undrawn row", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  /* scrolls through every page of the session; WebKit on Linux animates
     each wheel's scroll, so this outlasts the default limit there */
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await panel.hover();
  /* older pages first, so most of the session is boxes */
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(150);
  }
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, 1500);
    await page.waitForTimeout(100);
  }
  await page.waitForTimeout(500);

  for (const fraction of [0.5, 0.1, 0.8, 0.05]) {
    const { boxes, inView } = await page.evaluate(async (fraction) => {
      const scroller = document
        .querySelector('[data-testid="transcript-panel"]')!
        .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const boxesInView = () => {
        const view = scroller.getBoundingClientRect();
        return [...scroller.querySelectorAll('[data-window-row="box"]')].filter(
          (box) => {
            const r = box.getBoundingClientRect();
            return r.bottom > view.top && r.top < view.bottom;
          },
        ).length;
      };
      const boxes = scroller.querySelectorAll('[data-window-row="box"]').length;
      scroller.scrollTop = fraction * (scroller.scrollHeight - scroller.clientHeight);
      const inView: number[] = [];
      for (let i = 0; i < 12; i += 1) {
        await new Promise((r) => requestAnimationFrame(r));
        inView.push(boxesInView());
      }
      return { boxes, inView };
    }, fraction);
    expect(boxes).toBeGreaterThan(50);
    expect(inView).toEqual(inView.map(() => 0));
    await page.waitForTimeout(400);
  }
});
