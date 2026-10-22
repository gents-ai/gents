import type { Locator, Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

type Cost = {
  rects: number;
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

async function cost(
  viewport: Locator,
  act: "scroll" | "chunk" | "jump",
): Promise<Cost> {
  return viewport.evaluate(async (scroller, act) => {
    const rect = Element.prototype.getBoundingClientRect;
    const point = document.elementFromPoint;
    if (act === "scroll") document.elementFromPoint = () => null;
    const position = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    let rects = 0;
    let writes = 0;
    let scrolls = 0;
    const onScroll = () => {
      scrolls += 1;
    };
    scroller.addEventListener("scroll", onScroll);
    const height = scroller.scrollHeight;
    Element.prototype.getBoundingClientRect = function (this: Element) {
      rects += 1;
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
      } else {
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
        writes,
        scrolls,
        rows: scroller.querySelectorAll("[data-row-key]").length,
        loaded: Number(
          scroller.querySelector("[data-transcript-list]")?.getAttribute("data-rows") ??
            0,
        ),
        grew: scroller.scrollHeight > height,
      };
    } finally {
      document.elementFromPoint = point;
      scroller.removeEventListener("scroll", onScroll);
      Element.prototype.getBoundingClientRect = rect;
      delete (scroller as Partial<HTMLElement>).scrollTop;
    }
  }, act);
}

/* a scroll's cost, as the middle of five scroll events: other work that
   lands while one is measured, a page read ahead or a write at rest, is
   not the scroll's own */
async function scrollCost(viewport: Locator): Promise<Cost> {
  const costs: Cost[] = [];
  for (let n = 0; n < 5; n += 1) costs.push(await cost(viewport, "scroll"));
  return costs.sort((a, b) => a.rects - b.rects)[2]!;
}

async function loadEveryPage(page: Page) {
  await page.getByTestId("transcript-panel").hover();
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(150);
  }
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

async function backToBottom(page: Page) {
  await page.getByRole("button", { name: "Back to bottom" }).click();
  await expect(scroller(page)).toHaveAttribute("data-following", "true");
  await page.waitForTimeout(300);
}

test.describe("what a transcript scroll costs", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    test.slow();
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
      scroll: await scrollCost(viewport),
      chunk: await cost(viewport, "chunk"),
    };

    await loadEveryPage(page);
    const long = {
      scroll: await scrollCost(viewport),
      chunk: await cost(viewport, "chunk"),
    };
    testInfo.annotations.push({
      type: "cost",
      description: JSON.stringify({ short, long }),
    });
    /* the transcript holds far more rows; it draws about as many. The rows
       read ahead before the first measure vary by a page */
    expect(long.scroll.loaded).toBeGreaterThanOrEqual(short.scroll.loaded * 3);
    expect(long.scroll.rows).toBeLessThanOrEqual(short.scroll.rows + 12);

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
    const jump = await cost(viewport, "jump");
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
    const short = await cost(viewport, "chunk");
    await loadEveryPage(page);
    await backToBottom(page);
    const long = await cost(viewport, "chunk");
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
