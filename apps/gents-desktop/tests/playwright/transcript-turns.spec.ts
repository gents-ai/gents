import { composer, expect, gotoHarness, sendButton, test } from "./desktopTest";

test.describe("moving between turns", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start", { exact: false })
      .last()
      .waitFor();
  });

  const current = (page: import("@playwright/test").Page) =>
    page.evaluate(() => {
      const scroller = document.querySelector<HTMLElement>(
        '[data-slot="scroll-area-viewport"][data-following]',
      )!;
      const top = scroller.getBoundingClientRect().top;
      const turns = Array.from(scroller.querySelectorAll<HTMLElement>("[data-turn]"));
      const at = turns
        .filter((row) => row.getBoundingClientRect().top <= top + 72.5)
        .at(-1);
      return at
        ? {
            key: at.dataset.timelineKey!,
            offset: Math.round(at.getBoundingClientRect().top - top),
            following: scroller.dataset.following,
          }
        : null;
    });

  test("brings the previous turn to the reading line, then the next", async ({
    page,
  }) => {
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "true");
    const start = await current(page);
    await page.getByTestId("transcript-panel").hover();
    await page.keyboard.press("Alt+Meta+ArrowUp");
    await expect.poll(() => current(page).then((c) => c?.following)).toBe("false");
    const up = (await current(page))!;
    expect(up.key).not.toBe(start?.key);

    expect(Math.abs(up.offset - 56)).toBeLessThanOrEqual(1);

    await page.keyboard.press("Alt+Meta+ArrowUp");
    await expect.poll(() => current(page).then((c) => c?.key)).not.toBe(up.key);
    const upAgain = (await current(page))!;
    expect(Math.abs(upAgain.offset - 56)).toBeLessThanOrEqual(1);

    await page.keyboard.press("Alt+Meta+ArrowDown");
    await expect.poll(() => current(page).then((c) => c?.key)).toBe(up.key);
    expect(Math.abs((await current(page))!.offset - 56)).toBeLessThanOrEqual(1);
  });
});

/* With the preference on, a sent turn settles near the top of the view and
   the reply fills in beneath it; the view follows again once it has. */
test.describe("a sent turn kept near the top", () => {
  test("holds the turn at the reading line while the reply fills the view, then follows", async ({
    page,
  }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    /* the preference, as the store keeps it, then the harness again with it */
    await page.evaluate(() =>
      window.localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: { sentTurn: "anchor" }, version: 1 }),
      ),
    );
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    const panel = page.getByTestId("transcript-panel");
    await panel.getByText("stream-start", { exact: false }).last().waitFor();
    const viewport = page.locator('[data-slot="scroll-area-viewport"][data-following]');
    await expect(viewport).toHaveAttribute("data-following", "true");
    await page.waitForTimeout(400);

    /* the last turn ends and the person sends: the app draws its own copy
       of the message first, and the view goes to that */
    await page.evaluate(() =>
      window.__GENTS_MOBILE_PERFORMANCE__!.endReply({
        live: "drop",
        saved: true,
        completed: true,
      }),
    );
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.holdSends());
    await composer(page).fill("again");
    await sendButton(page).click();
    const turn = panel.locator("[data-turn]").last();
    await expect(turn).toContainText("again");
    /* the view goes to the turn once the session makes it the latest
       request, which the bridge's next read does */
    /* Driver round trips can outlast the glide; geometry must be sampled
       inside the page before the source update, including across row replacement. */
    const seen = await page.evaluate(
      () =>
        new Promise<{ from: number; offsets: number[] }>((resolve) => {
          const scroller = document.querySelector<HTMLElement>(
            '[data-slot="scroll-area-viewport"][data-following]',
          )!;
          const offset = () =>
            Math.round(
              Array.from(scroller.querySelectorAll<HTMLElement>("[data-turn]"))
                .at(-1)!
                .getBoundingClientRect().top - scroller.getBoundingClientRect().top,
            );
          const from = offset();
          const offsets: number[] = [];
          const deadline = performance.now() + 2000;
          const sample = () => {
            const at = offset();
            offsets.push(at);
            if (
              (scroller.dataset.following === "false" && at <= 89) ||
              performance.now() >= deadline
            ) {
              resolve({ from, offsets });
            } else requestAnimationFrame(sample);
          };
          requestAnimationFrame(sample);
          window.__GENTS_MOBILE_PERFORMANCE__!.userTurn("pending");
        }),
    );
    await expect(viewport).toHaveAttribute("data-following", "false");
    const offset = async () =>
      Math.round((await turn.boundingBox())!.y - (await viewport.boundingBox())!.y);
    await expect.poll(offset).toBeLessThanOrEqual(89);
    expect(await offset()).toBeGreaterThanOrEqual(87);
    expect(
      seen.offsets.filter((y) => y > 100 && y < seen.from - 1),
      JSON.stringify(seen),
    ).not.toHaveLength(0);
    /* the last turn's block has room for the reply: a minimum height
       the owner set once, from the view, not from the reply */
    const room = () =>
      panel.evaluate(
        (node) =>
          parseFloat(
            node
              .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!
              .style.getPropertyValue("--turn-room"),
          ) || 0,
      );
    const roomAtSend = await room();
    expect(roomAtSend).toBeGreaterThan(400);

    /* the bridge shows the message as its saved row, and the reply begins:
       the hold and its room stay with the turn through every row that
       stands for it */
    for (const step of ["saved"] as const) {
      await page.evaluate(
        (step) => window.__GENTS_MOBILE_PERFORMANCE__!.userTurn(step),
        step,
      );
      await page.waitForTimeout(300);
      await expect(viewport).toHaveAttribute("data-following", "false");
      expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);
      expect(await room()).toBe(roomAtSend);
    }
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.beginReply());
    await page.waitForTimeout(200);
    expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);

    /* the reply streams in under the turn; the turn does not move */
    for (let n = 0; n < 6; n += 1) {
      await page.evaluate(
        (n) =>
          window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
            `\n\nParagraph ${n} of the reply, long enough to wrap onto a second line in the column and then some more.`,
          ),
        n,
      );
      await page.waitForTimeout(120);
      if ((await viewport.getAttribute("data-following")) === "false")
        expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);
    }
    /* the turn takes a tool step while room is left: still held */
    await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.liveTool("running"));
    await page.waitForTimeout(200);
    await expect(viewport).toHaveAttribute("data-following", "false");
    expect(Math.abs((await offset()) - 88)).toBeLessThanOrEqual(1);

    /* enough reply to fill the view: a working turn is followed from the
       layout where it crosses the foot (a plain answer would stay held;
       the unit tests cover that) */
    for (let n = 6; n < 30; n += 1) {
      await page.evaluate(
        (n) =>
          window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
            `\n\nParagraph ${n} of the reply, long enough to wrap onto a second line in the column and then some more.`,
          ),
        n,
      );
      await page.waitForTimeout(60);
      if ((await viewport.getAttribute("data-following")) === "true") break;
    }
    await expect(viewport).toHaveAttribute("data-following", "true", {
      timeout: 10_000,
    });
    /* the room is a minimum the reply has long exceeded: it stays as set */
    expect(await room()).toBe(roomAtSend);
  });
});
