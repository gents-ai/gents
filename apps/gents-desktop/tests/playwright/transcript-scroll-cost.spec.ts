import type { Locator, Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

type Cost = {
  rects: number;
  rowRects: number;
  measuredRows: number;
  writes: number;
  /* rows drawn, and rows the transcript holds */
  rows: number;
  loaded: number;
  grew: boolean;
  scrolls: number;
};

function scroller(page: Page) {
  return page.locator('[data-slot="scroll-area-viewport"][data-following]');
}

/** Measure the action before idle timers or older-page delivery can add unrelated work. */
async function cost(
  page: Page,
  viewport: Locator,
  act: "scroll" | "chunk" | "jump",
): Promise<Cost> {
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.holdOlderPages());
  await page.clock.pauseAt(await page.evaluate(() => Date.now() + 60_000));
  await page.clock.runFor(1000);
  let finished = false;
  const measurement = viewport
    .evaluate(async (scroller, act) => {
      const rect = Element.prototype.getBoundingClientRect;
      const point = document.elementFromPoint;
      if (act === "scroll") document.elementFromPoint = () => null;
      const position = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
      let rects = 0;
      let rowRects = 0;
      const measuredRows = new Set<Element>();
      let writes = 0;
      let scrolls = 0;
      const onScroll = () => {
        scrolls += 1;
      };
      scroller.addEventListener("scroll", onScroll);
      const height = scroller.scrollHeight;
      Element.prototype.getBoundingClientRect = function (this: Element) {
        rects += 1;
        if (this.matches("[data-row-key]")) {
          rowRects += 1;
          measuredRows.add(this);
        }
        return rect.call(this);
      };
      Object.defineProperty(scroller, "scrollTop", {
        configurable: true,
        get: () => position.get!.call(scroller),
        set: (value: number) => {
          writes += 1;
          position.set!.call(scroller, value);
        },
      });
      try {
        if (act !== "chunk") {
          const before = position.get!.call(scroller) as number;
          const delta =
            act === "jump" ? scroller.clientHeight * 2 : before >= 20 ? -20 : 20;
          await new Promise<void>((resolve) => {
            scroller.addEventListener("scroll", () => resolve(), { once: true });
            scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: delta }));
            position.set!.call(scroller, before + delta);
          });
          scroller.dataset.measuringCost = "acted";
        } else {
          scroller.dataset.measuringCost = "acted";
          window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
            "\n\nMore of the reply, long enough to wrap onto a second line in the column.",
          );
          const deadline = performance.now() + 3000;
          while (scroller.scrollHeight <= height && performance.now() < deadline)
            await new Promise((resolve) => requestAnimationFrame(resolve));
        }
        await new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve)),
        );
        return {
          rects,
          rowRects,
          measuredRows: measuredRows.size,
          writes,
          scrolls,
          rows: scroller.querySelectorAll("[data-row-key]").length,
          loaded: Number(
            scroller
              .querySelector("[data-transcript-list]")
              ?.getAttribute("data-rows") ?? 0,
          ),
          grew: scroller.scrollHeight > height,
        };
      } finally {
        document.elementFromPoint = point;
        scroller.removeEventListener("scroll", onScroll);
        Element.prototype.getBoundingClientRect = rect;
        delete (scroller as Partial<HTMLElement>).scrollTop;
        delete scroller.dataset.measuringCost;
      }
    }, act)
    .finally(() => {
      finished = true;
    });
  void measurement.catch(() => {});
  try {
    /* the browser delivers the scroll in real time while the paused clock
       only moves when told: frames advance once the scroll has landed, so
       the measured frames never reach the rest that writes the carried
       position and draws another stretch of rows */
    await expect.poll(() => viewport.getAttribute("data-measuring-cost")).toBe("acted");
    for (let frame = 0; frame < 200 && !finished; frame += 1)
      await page.clock.runFor(16);
    expect(finished).toBe(true);
    return await measurement;
  } finally {
    await page.clock.resume();
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.releaseOlderPages());
  }
}

/* a scroll's cost, as the middle of five scroll events: other work that
   lands while one is measured, a page read ahead or a write at rest, is
   not the scroll's own */
async function scrollCost(page: Page, viewport: Locator): Promise<Cost> {
  const costs: Cost[] = [];
  for (let n = 0; n < 5; n += 1) costs.push(await cost(page, viewport, "scroll"));
  return costs.sort((a, b) => a.rects - b.rects)[2]!;
}

async function loadEveryPage(page: Page) {
  await page.getByTestId("transcript-panel").hover();
  const expected = await page.evaluate(
    () => window.__GENTS_MOBILE_PERFORMANCE__!.fixture.largeSessionTimelineItems,
  );
  await expect
    .poll(
      async () => {
        const loaded = await scroller(page)
          .locator("[data-transcript-list]")
          .getAttribute("data-rows");
        if (Number(loaded) < expected) await page.mouse.wheel(0, -1500);
        return Number(loaded);
      },
      { timeout: 30_000, intervals: [150] },
    )
    .toBeGreaterThanOrEqual(expected);
  await page.waitForTimeout(600);
  await expect
    .poll(() =>
      scroller(page)
        .locator("[data-transcript-content]")
        .evaluate((content: HTMLElement) => content.style.marginTop),
    )
    .toBe("");
  await page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      ),
  );
}

/* rows near the foot are measured as they mount on the way back, so a
   chunk's cost is taken once the view rests at the foot and the content
   holds its height for a few frames */
async function backToBottom(page: Page) {
  await page.getByRole("button", { name: "Back to bottom" }).click();
  await expect(scroller(page)).toHaveAttribute("data-following", "true");
  await expect
    .poll(() =>
      scroller(page).evaluate(async (scroller) => {
        const height = scroller.scrollHeight;
        for (let frame = 0; frame < 6; frame += 1)
          await new Promise((resolve) => requestAnimationFrame(resolve));
        const foot = scroller.scrollHeight - scroller.clientHeight;
        return (
          scroller.scrollHeight === height && Math.abs(scroller.scrollTop - foot) < 1
        );
      }),
    )
    .toBe(true);
}

test.describe("what a transcript scroll costs", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    test.slow();
    await page.clock.install();
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start", { exact: false })
      .last()
      .waitFor();
  });

  test("bounds fallback measurements per native scroll event as the session grows", async ({
    page,
  }, testInfo) => {
    const viewport = scroller(page);

    await page.getByTestId("transcript-panel").hover();
    await page.mouse.wheel(0, -600);
    await expect(viewport).toHaveAttribute("data-following", "false");
    await page.waitForTimeout(800);
    const short = {
      scroll: await scrollCost(page, viewport),
      chunk: await cost(page, viewport, "chunk"),
    };

    await loadEveryPage(page);
    const long = {
      scroll: await scrollCost(page, viewport),
      chunk: await cost(page, viewport, "chunk"),
    };
    testInfo.annotations.push({
      type: "cost",
      description: JSON.stringify({ short, long }),
    });
    /* the transcript holds far more rows and still draws about a view's
       worth: how many depends on where the reader stopped, not on how many
       are held. The rows read ahead before the first measure vary by a page */
    expect(long.scroll.loaded).toBeGreaterThanOrEqual(short.scroll.loaded * 3);
    expect(long.scroll.rows).toBeLessThanOrEqual(40);

    /* a reader's scroll finds the row under them by halving over the drawn
       rows; a streamed chunk measures the held row and the view. Far up a
       long session the reply grows in a row that is not drawn */
    expect(short.chunk.grew).toBe(true);
    for (const at of [short, long]) {
      expect(at.scroll.rects).toBeLessThanOrEqual(16);
      expect(at.chunk.rects).toBeLessThanOrEqual(16);
      expect(at.scroll.writes).toBe(0);
    }
    /* and none of it grows with the rows held: more rows drawn add a
       halving step, and a row drawn by the scroll is measured once */
    expect(long.scroll.rects).toBeLessThanOrEqual(short.scroll.rects + 6);
    expect(long.chunk.rects).toBeLessThanOrEqual(short.chunk.rects + 4);
  });

  test("bounds layout reads when jumping through loaded history", async ({
    page,
  }, testInfo) => {
    const viewport = scroller(page);
    await loadEveryPage(page);
    const jump = await cost(page, viewport, "jump");
    testInfo.annotations.push({ type: "cost", description: JSON.stringify(jump) });
    /* each row drawn on the way is measured as it mounts, and the reader's
       place is found by halving: the reads follow the rows drawn, never
       the rows the session holds */
    expect(jump.loaded).toBeGreaterThan(400);
    expect(jump.rows).toBeLessThanOrEqual(40);
    expect(jump.rects).toBeLessThanOrEqual(jump.rows * 3);
    await expect
      .poll(() =>
        viewport.evaluate((scroller) => {
          const view = scroller.getBoundingClientRect();
          return Array.from(scroller.querySelectorAll("[data-row-key]")).some((row) => {
            const box = row.getBoundingClientRect();
            return box.bottom > view.top && box.top < view.bottom;
          });
        }),
      )
      .toBe(true);
  });

  test("writes the position at most once per streamed chunk while following", async ({
    page,
  }, testInfo) => {
    const viewport = scroller(page);
    await expect(viewport).toHaveAttribute("data-following", "true");
    await page.waitForTimeout(500);
    const short = await cost(page, viewport, "chunk");
    await loadEveryPage(page);
    await backToBottom(page);
    const long = await cost(page, viewport, "chunk");
    testInfo.annotations.push({
      type: "cost",
      description: JSON.stringify({ short, long }),
    });
    for (const at of [short, long]) {
      expect(at.grew).toBe(true);

      expect(at.rects).toBeLessThanOrEqual(8);
      expect(at.writes).toBeLessThanOrEqual(2);
    }
    expect(long.rects).toBeLessThanOrEqual(short.rects + 2);
  });
});
