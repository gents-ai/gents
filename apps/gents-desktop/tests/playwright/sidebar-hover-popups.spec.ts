import type { Locator } from "@playwright/test";
import { expect, gotoHarness, test, type Page } from "./desktopTest";

async function hoverMode(page: Page) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await gotoHarness(page, "active-nodes");
  await page.evaluate(() =>
    localStorage.setItem(
      "gents-preferences",
      JSON.stringify({ state: { nav: "hover" }, version: 1 }),
    ),
  );
  await page.reload();
  await expect(page.getByTestId("app-shell")).toBeVisible();
}

const centre = async (locator: Locator) => {
  const box = (await locator.boundingBox())!;
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
};

/* the panel and its node switcher, opened from the rail by the pointer */
async function openSwitcher(page: Page) {
  const panel = page.locator("[data-ground=raised]");
  const rail = await centre(page.locator("nav").first());
  await page.mouse.move(rail.x, rail.y);
  await expect(panel).toHaveAttribute("aria-hidden", "false");
  const button = panel.getByRole("button", { name: "Switch node" });
  const at = await centre(button);
  await page.mouse.move(at.x, at.y, { steps: 4 });
  await page.mouse.click(at.x, at.y);
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  return { panel, menu };
}

test.describe("the hover panel and what opens from it", () => {
  /* it sets its own 1440×900 window, so the laptop and narrow projects
     would only repeat the desktop Chromium run */
  test.beforeEach(({}, info) => {
    test.skip(
      info.project.name === "chromium-laptop" ||
        info.project.name === "chromium-narrow",
      "sets its own desktop window",
    );
  });
  test("stay open together while the pointer is on either, and close together", async ({
    page,
  }) => {
    await hoverMode(page);
    const { panel, menu } = await openSwitcher(page);
    const item = await centre(menu.getByRole("menuitemradio").last());
    await page.mouse.move(item.x, item.y, { steps: 8 });
    await page.waitForTimeout(400);
    await expect(panel).toHaveAttribute("aria-hidden", "false");
    await expect(menu).toBeVisible();
    await page.mouse.move(1100, 500, { steps: 8 });
    await expect(menu).toBeHidden();
    await expect(panel).toHaveAttribute("aria-hidden", "true");
  });

  test("closes once a choice from its menu is made and the pointer moves away", async ({
    page,
  }) => {
    await hoverMode(page);
    const { panel, menu } = await openSwitcher(page);
    const other = menu.getByRole("menuitemradio", { checked: false }).first();
    const item = await centre(other);
    await page.mouse.move(item.x, item.y, { steps: 8 });
    await page.mouse.click(item.x, item.y);
    await expect(menu).toBeHidden();
    /* the menu went from under the pointer, which now rests outside the panel */
    await page.mouse.move(item.x + 40, item.y + 10, { steps: 4 });
    await expect(panel).toHaveAttribute("aria-hidden", "true");
  });

  test("close together when the pointer goes back to the panel and leaves it", async ({
    page,
  }) => {
    await hoverMode(page);
    const { panel, menu } = await openSwitcher(page);
    const row = await centre(panel.getByRole("link", { name: /New session/ }));
    await page.mouse.move(row.x, row.y, { steps: 8 });
    await page.waitForTimeout(300);
    await expect(panel).toHaveAttribute("aria-hidden", "false");
    await page.mouse.move(1100, row.y, { steps: 8 });
    await expect(panel).toHaveAttribute("aria-hidden", "true");
    await expect(menu).toBeHidden();
  });

  test("close together from a hidden sidebar's peek", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await gotoHarness(page, "active-nodes");
    await page.evaluate(() =>
      localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: { nav: "expanded", navHidden: true }, version: 1 }),
      ),
    );
    await page.reload();
    const peek = page.getByTestId("sidebar-peek");
    await page.mouse.move(400, 400);
    await page.mouse.move(2, 400);
    await expect(peek).toHaveAttribute("aria-hidden", "false");
    /* it slides in; its switcher is where it rests */
    await page.waitForTimeout(300);
    const at = await centre(peek.getByRole("button", { name: "Switch node" }));
    await page.mouse.move(at.x, at.y, { steps: 4 });
    await page.mouse.click(at.x, at.y);
    const menu = page.getByRole("menu");
    await expect(menu).toBeVisible();
    await page.mouse.move(1100, 500, { steps: 8 });
    await expect(peek).toHaveAttribute("aria-hidden", "true");
    await expect(menu).toBeHidden();
  });

  test("draws over a plugin page's frame", async ({ page }) => {
    await hoverMode(page);
    const nav = page.locator("nav").first();
    const railEdge = (await nav.boundingBox())!;
    /* a frame where plugin pages' frames are staged, over the page beside
       the rail, as a plugin page's frame sits */
    await page.evaluate(async (left) => {
      const { stageElement } = await import(
        /* @vite-ignore */ `${"/src/ui/contrib/frame-stage.ts"}`
      );
      const frame = document.createElement("iframe");
      frame.style.cssText = `position:fixed;top:0;bottom:0;left:${left}px;right:0;width:calc(100vw - ${left}px);height:100vh;border:0;pointer-events:auto`;
      stageElement().appendChild(frame);
    }, railEdge.x + railEdge.width);
    const panel = page.locator("[data-ground=raised]");
    const rail = await centre(nav);
    await page.mouse.move(rail.x, rail.y);
    await expect(panel).toHaveAttribute("aria-hidden", "false");
    const box = (await panel.boundingBox())!;
    const onTop = (x: number, y: number) =>
      page.evaluate(
        ([x, y]) => document.elementFromPoint(x, y)?.closest("iframe") === null,
        [x, y],
      );
    expect(await onTop(box.x + box.width - 24, box.y + box.height / 2)).toBe(true);
    /* and the frame still covers the page beside it */
    expect(await onTop(box.x + box.width + 200, box.y + box.height / 2)).toBe(false);
  });

  test("a click begun on the rail as its panel opens is not lost", async ({ page }) => {
    /* React draws an update from a timer in a task posted on a message
       channel, and a browser runs input ahead of such a task. Hold those
       tasks to put the press between the open timer and the panel's draw,
       as a busy page does */
    await page.clock.install();
    await hoverMode(page);
    /* the page's own message ports, the scheduler's among them */
    await page.evaluate(() => {
      const post = MessagePort.prototype.postMessage;
      const held: (() => void)[] = [];
      const hold = { on: false };
      MessagePort.prototype.postMessage = function (
        this: MessagePort,
        ...args: Parameters<MessagePort["postMessage"]>
      ) {
        if (hold.on) held.push(() => post.apply(this, args));
        else post.apply(this, args);
      } as MessagePort["postMessage"];
      Object.assign(window, {
        __holdDraws: () => (hold.on = true),
        __releaseDraws: () => {
          hold.on = false;
          held.splice(0).forEach((draw) => draw());
        },
      });
    });
    await page.clock.pauseAt(await page.evaluate(() => Date.now() + 1000));
    const panel = page.locator("[data-ground=raised]");
    await expect(panel).toHaveAttribute("aria-hidden", "true");
    const link = await centre(
      page.locator("nav").first().getByRole("link", { name: "New session" }),
    );
    const fresh = page.getByText(/Start a new chat with/);
    await expect(fresh).toHaveCount(0);
    await page.mouse.move(link.x, link.y);
    /* the open timer fires, and its draw waits */
    await page.evaluate(() =>
      (window as unknown as { __holdDraws: () => void }).__holdDraws(),
    );
    await page.clock.runFor(100);
    await page.mouse.down();
    await page.evaluate(
      () =>
        new Promise<void>((resolve) => {
          (window as unknown as { __releaseDraws: () => void }).__releaseDraws();
          const channel = new MessageChannel();
          channel.port1.onmessage = () => resolve();
          channel.port2.postMessage(null);
        }),
    );
    await page.mouse.up();
    await page.clock.resume();
    await expect(fresh).toBeVisible();
  });
});
