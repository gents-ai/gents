import { expect, gotoHarness, test } from "./desktopTest";

/* Scrolling up a long session at a steady pace, older pages arrive before
   the reader reaches the top: the view does not sit at the top waiting
   for each page while there are older ones to load. */
test("scrolling up a long session does not wait at the top for each page", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  /* scrolls up for 12 s; WebKit on Linux animates each wheel's scroll, so
     this outlasts the default limit there */
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  /* a page read takes as long as a bridge read of the session does */
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(400),
  );

  await page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="transcript-panel"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const frames: { top: number; rows: number; t: number }[] = [];
    const tick = () => {
      frames.push({
        top: scroller.scrollTop,
        rows: scroller.querySelectorAll("[data-timeline-key]").length,
        t: performance.now(),
      });
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __olderFrames: frames });
  });

  await panel.hover();
  /* about 2,400 px a second for 12 s, a reader moving up through the session */
  for (let i = 0; i < 240; i += 1) {
    await page.mouse.wheel(0, -120);
    await page.waitForTimeout(50);
  }

  const frames = await page.evaluate(
    () =>
      (
        window as unknown as {
          __olderFrames: { top: number; rows: number; t: number }[];
        }
      ).__olderFrames,
  );
  const allRows = Math.max(...frames.map((f) => f.rows));
  /* time spent at the top while more rows were still to come */
  let waited = 0;
  for (let i = 1; i < frames.length; i += 1)
    if (frames[i].top <= 2 && frames[i].rows < allRows)
      waited += frames[i].t - frames[i - 1].t;
  console.log(
    `rows ${frames[0].rows} -> ${allRows}, waited at the top ${Math.round(waited)} ms`,
  );
  expect(allRows).toBeGreaterThanOrEqual(frames[0].rows + 160);
  expect(waited).toBeLessThan(150);
});

/* A page now lands while the reader is still views below the top; the row
   they are reading stays where it is. */
test("a page landing views above the reader leaves their row in place", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(400),
  );

  const before = await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    /* as a reader does: their own wheel, then the scroll it makes, ending
       two views below the top */
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -1, bubbles: true }));
    scroller.scrollTop = 2 * scroller.clientHeight;
    const view = scroller.getBoundingClientRect();
    const row = document
      .elementFromPoint(view.left + view.width / 2, view.top + view.height / 2)!
      .closest<HTMLElement>("[data-timeline-key]")!;
    return {
      key: row.dataset.timelineKey!,
      top: row.getBoundingClientRect().top,
      rows: scroller.querySelectorAll("[data-timeline-key]").length,
    };
  });
  const row = page.locator(`[data-timeline-key="${before.key}"]`);
  await expect
    .poll(() =>
      panel.evaluate(
        (node) =>
          node
            .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!
            .querySelectorAll("[data-timeline-key]").length,
      ),
    )
    .toBeGreaterThan(before.rows);
  await page.waitForTimeout(200);
  const after = await row.evaluate((el) => el.getBoundingClientRect().top);
  expect(Math.abs(after - before.top)).toBeLessThan(1);
});

/* WebKit drops a write to the position made while its own scrolling is under
   way, and stops drawing the transcript until a later write takes. A page
   that lands while the reader scrolls is kept out of sight above the
   content instead; the position is written only once the view rests or
   reaches the top. */
test("a page landing while the reader scrolls is not put back with a write until the view rests", async ({
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
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(400),
  );

  /* every write to the position once the reader starts scrolling (opening
     the session pins it to the foot): how far it moved the view, how long
     after the reader's last wheel or scroll, and where it was */
  await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    const writes: { by: number; sinceMoved: number; from: number }[] = [];
    let movedAt = -Infinity;
    let reading = false;
    /* the wheel counts as a move too: WebKitGTK can start its scroll late,
       so a write before any scroll event must not read as after a rest */
    scroller.addEventListener(
      "wheel",
      () => {
        reading = true;
        movedAt = performance.now();
      },
      { capture: true, passive: true },
    );
    scroller.addEventListener("scroll", () => (movedAt = performance.now()), {
      capture: true,
      passive: true,
    });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => native.get!.call(scroller),
      set: (value: number) => {
        const from = native.get!.call(scroller) as number;
        if (reading)
          writes.push({
            by: value - from,
            sinceMoved: performance.now() - movedAt,
            from,
          });
        native.set!.call(scroller, value);
      },
    });
    Object.assign(window, { __writes: writes });
  });

  await panel.hover();
  for (let i = 0; i < 60; i += 1) {
    await page.mouse.wheel(0, -120);
    await page.waitForTimeout(40);
  }
  /* the rest gate is a second long: wait the hidden rows out, not a fixed
     delay that a loaded runner can outpace */
  await expect
    .poll(() =>
      panel.evaluate(
        (node) =>
          getComputedStyle(
            node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!
              .firstElementChild!,
          ).marginTop,
      ),
    )
    .toBe("0px");

  const { writes, rows, hidden } = await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    return {
      writes: (
        window as unknown as {
          __writes: { by: number; sinceMoved: number; from: number }[];
        }
      ).__writes,
      rows: scroller.querySelectorAll("[data-timeline-key]").length,
      hidden: getComputedStyle(scroller.firstElementChild!).marginTop,
    };
  });
  /* a write that puts a page back moves the view by the page's height */
  const pagesPutBack = writes.filter((w) => Math.abs(w.by) > 450);
  expect(rows).toBeGreaterThan(40);
  expect(pagesPutBack.length).toBeGreaterThan(0);
  expect(pagesPutBack.filter((w) => w.sinceMoved < 1000 && w.from > 0)).toEqual([]);
  /* rested: nothing is left out of sight */
  expect(hidden).toBe("0px");
});

/* One wheel up the page, an older page landing during its animation, and
   the wheel's whole travel still arriving: a write to the position while
   WebKit is scrolling stops the animation, which is the loss the issue's
   traces caught (a 150 ms stillness gate firing mid-flight on WebKitGTK).
   The wheel carries the view its full 1200 px, which the landing and the
   write that puts it back never touch. */
test("a page landing mid-wheel keeps the wheel's whole travel", async ({
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
  /* a page read lands partway into the wheel's animation */
  await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(200),
  );

  /* the wheel, the view's own moves, and every write to the position, on
     one clock; a write remembers how long after the reader's last wheel
     or scroll it came */
  await panel.evaluate((node) => {
    const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const native = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    const writes: { by: number; sinceMoved: number; from: number }[] = [];
    let movedAt = -Infinity;
    let reading = false;
    scroller.addEventListener(
      "wheel",
      () => {
        reading = true;
        movedAt = performance.now();
      },
      { capture: true, passive: true },
    );
    scroller.addEventListener("scroll", () => (movedAt = performance.now()), {
      capture: true,
      passive: true,
    });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => native.get!.call(scroller),
      set: (value: number) => {
        const from = native.get!.call(scroller) as number;
        if (reading)
          writes.push({
            by: value - from,
            sinceMoved: performance.now() - movedAt,
            from,
          });
        native.set!.call(scroller, value);
      },
    });
    Object.assign(window, { __writes: writes });
  });

  /* the row under the reading line, and that same row by its key once the
     wheel has carried it down the screen: the landing and the write that
     puts it back are visually neutral, so the row moves only as far as
     the wheel carries the view */
  const rowAtLine = () =>
    panel.evaluate((node) => {
      const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const top = scroller.getBoundingClientRect().top;
      const row = Array.from(
        scroller.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      ).find((r) => r.getBoundingClientRect().bottom > top + 200)!;
      return {
        key: row.dataset.timelineKey!,
        offset: Math.round(row.getBoundingClientRect().top - top),
      };
    });
  const rowOffset = (key: string) =>
    panel.evaluate((node, key) => {
      const scroller = node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const top = scroller.getBoundingClientRect().top;
      const row = scroller.querySelector<HTMLElement>(`[data-timeline-key="${key}"]`)!;
      return Math.round(row.getBoundingClientRect().top - top);
    }, key);
  const rowCount = () =>
    panel.evaluate(
      (node) =>
        node
          .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!
          .querySelectorAll("[data-timeline-key]").length,
    );
  const scrollTop = () =>
    panel.evaluate(
      (node) =>
        node.closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!.scrollTop,
    );
  const stillTop = async () => {
    let still = 0;
    let last = await scrollTop();
    const deadline = Date.now() + 5000;
    while (still < 500 && Date.now() < deadline) {
      await page.waitForTimeout(50);
      const next = await scrollTop();
      still = next === last ? still + 50 : 0;
      last = next;
    }
    return last;
  };

  await panel.hover();
  const rowsBefore = await rowCount();
  const reader = await rowAtLine();
  await page.mouse.wheel(0, -1200);
  /* the page lands out of sight, and the write that puts it back is the
     rest-gated one this waits for: nothing else moves the position */
  await expect
    .poll(() =>
      panel.evaluate(
        () =>
          (
            window as unknown as {
              __writes: { by: number; sinceMoved: number; from: number }[];
            }
          ).__writes.filter((w) => w.from > 0 && Math.abs(w.by) > 450).length,
      ),
    )
    .toBeGreaterThan(0);
  await stillTop();
  const writes = await panel.evaluate(
    () =>
      (
        window as unknown as {
          __writes: { by: number; sinceMoved: number; from: number }[];
        }
      ).__writes,
  );
  /* a page landed and was put back, and every put-back write came at
     rest, a full second after the reader's last wheel or scroll */
  expect(await rowCount(), JSON.stringify(writes)).toBeGreaterThan(rowsBefore);
  const pagesPutBack = writes.filter((w) => w.from > 0 && Math.abs(w.by) > 450);
  expect(pagesPutBack.length, JSON.stringify(writes)).toBeGreaterThan(0);
  expect(
    pagesPutBack.filter((w) => w.sinceMoved < 1000),
    JSON.stringify(writes),
  ).toEqual([]);
  /* rested: nothing is left out of sight, and the wheel carried the
     reader's row the whole of its 1200 px */
  const travel = (await rowOffset(reader.key)) - reader.offset;
  console.log(`wheel travel ${travel} px, writes ${JSON.stringify(writes)}`);
  expect(travel).toBeGreaterThan(1140);
  expect(travel).toBeLessThan(1260);
});
