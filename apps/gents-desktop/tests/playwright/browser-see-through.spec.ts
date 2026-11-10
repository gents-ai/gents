import { expect, gotoHarness, test } from "./desktopTest";

/* The browser page is a native view beneath the app; here the native side
   is stubbed and the app's half is checked: the page's area left unpainted,
   the backgrounds painted back around it, and what the app draws over the
   page reported so its clicks stay with the app. */
test.describe("the browser page seen through the app", () => {
  test.beforeEach(({}, info) => {
    test.skip(info.project.name !== "chromium-desktop", "one desktop window is enough");
  });

  test("leaves the page's area to the page, and reports what the app draws over it", async ({
    page,
  }) => {
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

    await page.evaluate(async () => {
      const calls: { cmd: string; args: unknown }[] = [];
      Object.assign(window, { browserCalls: calls });
      const internals = ((
        window as unknown as { __TAURI_INTERNALS__?: Record<string, unknown> }
      ).__TAURI_INTERNALS__ ??= {});
      internals.invoke = async (cmd: string, args: unknown) => {
        calls.push({ cmd, args });
        return null;
      };
      const main = document.querySelector('[data-testid="pane"] main') as HTMLElement;
      const place = document.createElement("div");
      place.dataset.testid = "browser-view";
      place.style.cssText =
        "position:absolute;left:120px;top:40px;width:600px;height:500px";
      main.appendChild(place);
      const { seeThrough } = await import(
        /* @vite-ignore */ `${"/src/ui/screens/browser/seeThrough.ts"}`
      );
      Object.assign(window, { stopSeeThrough: seeThrough(place) });
    });

    type Placement = {
      open: { x: number; y: number; width: number; height: number };
      solids: { x: number; y: number; width: number; height: number }[];
    };
    const lastPlacement = () =>
      page.evaluate(() => {
        const calls = (
          window as unknown as {
            browserCalls: { cmd: string; args: { placement?: unknown } }[];
          }
        ).browserCalls;
        return (
          calls.filter((c) => c.cmd === "browser_place").at(-1)?.args.placement ?? null
        );
      }) as Promise<Placement | null>;

    await expect.poll(lastPlacement).not.toBeNull();
    const placement = (await lastPlacement())!;
    const box = (await page.getByTestId("browser-view").boundingBox())!;
    expect(placement.open.x).toBeCloseTo(box.x, 0);
    expect(placement.open.width).toBeGreaterThan(100);
    expect(placement.solids).toEqual([]);

    /* the shell stops painting, and a layer beneath paints it back */
    const shellPaint = () =>
      page
        .getByTestId("app-shell")
        .evaluate((el) => getComputedStyle(el).backgroundColor);
    expect(await shellPaint()).toBe("rgba(0, 0, 0, 0)");
    await expect(page.locator("[data-browser-underlay] > div")).not.toHaveCount(0);

    /* the sidebar's hover panel opens over the page, and is reported */
    const rail = (await page.locator("nav").first().boundingBox())!;
    await page.mouse.move(rail.x + rail.width / 2, rail.y + rail.height / 2);
    const panel = page.locator("[data-ground=raised]");
    await expect(panel).toHaveAttribute("aria-hidden", "false");
    const panelBox = (await panel.boundingBox())!;
    await expect
      .poll(async () =>
        ((await lastPlacement())?.solids ?? []).some(
          (s) =>
            Math.abs(s.x - panelBox.x) < 2 && Math.abs(s.width - panelBox.width) < 2,
        ),
      )
      .toBe(true);

    /* stopping puts every background back */
    await page.evaluate(() =>
      (window as unknown as { stopSeeThrough: () => void }).stopSeeThrough(),
    );
    expect(await shellPaint()).not.toBe("rgba(0, 0, 0, 0)");
    await expect(page.locator("[data-browser-underlay]")).toHaveCount(0);
  });
});
