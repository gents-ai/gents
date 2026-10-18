import { expect, gotoHarness, test } from "./desktopTest";

type Frame = {
  boxes: number;
  boxesInView: number;
  rows: number;
  height: number;
  hidden: number;
};

test("a long transcript shows no undrawn row while the reader scrolls up", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();

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
        hidden: -parseFloat(getComputedStyle(scroller.firstElementChild!).marginTop),
      });
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __windowFrames: seen });
  });

  await panel.hover();
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(150);
  }
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
  const resized = frames.filter(
    (f, i) =>
      i > 0 &&
      f.rows === frames[i - 1].rows &&
      f.hidden === frames[i - 1].hidden &&
      f.height !== frames[i - 1].height,
  );
  expect(resized).toEqual([]);
});

test("a jump to a far part of a long transcript shows no undrawn row", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await panel.hover();
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
