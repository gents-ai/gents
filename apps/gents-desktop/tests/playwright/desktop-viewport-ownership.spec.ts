import {
  expect,
  gotoHarness,
  openChat,
  openChatNavigation,
  openConfig,
  openConfigTab,
  test,
  type Page,
} from "./desktopTest";

const phoneViewports = [
  { width: 320, height: 568 },
  { width: 430, height: 932 },
] as const;

async function expectShellClipsToRouteOwner(page: Page) {
  const state = await page.evaluate(() => {
    const shell = document.querySelector<HTMLElement>(".app-shell");
    const route = document.querySelector<HTMLElement>("[data-testid='app-route-slot']");
    if (!shell || !route) throw new Error("shell ownership geometry missing");
    shell.scrollTop = 100;
    const routeElements = [route, ...route.querySelectorAll<HTMLElement>("*")];
    return {
      declaredOwners: routeElements
        .filter(
          (element) =>
            element.dataset.scrollOwner &&
            element.getClientRects().length > 0 &&
            !element.closest("details:not([open])"),
        )
        .map((element) => element.dataset.scrollOwner),
      rogueActiveScrollers: routeElements
        .filter((element) => {
          const overflowY = getComputedStyle(element).overflowY;
          return (
            /^(auto|scroll)$/.test(overflowY) &&
            element.scrollHeight > element.clientHeight + 1 &&
            !element.dataset.scrollOwner
          );
        })
        .map((element) => element.className),
      routeOwner: route.dataset.scrollOwner,
      routeOverflowY: getComputedStyle(route).overflowY,
      shellOverflowY: getComputedStyle(shell).overflowY,
      shellScrollTop: shell.scrollTop,
    };
  });
  expect(state).toEqual({
    declaredOwners: ["route"],
    rogueActiveScrollers: [],
    routeOwner: "route",
    routeOverflowY: "auto",
    shellOverflowY: "clip",
    shellScrollTop: 0,
  });
}

async function installVisualViewport(
  page: Page,
  geometry = { height: 500, width: 360, offsetTop: 24, offsetLeft: 8 },
) {
  await page.addInitScript((initialGeometry) => {
    const viewport = new EventTarget() as EventTarget & {
      height: number;
      width: number;
      offsetTop: number;
      offsetLeft: number;
    };
    Object.assign(viewport, initialGeometry);
    Object.defineProperty(window, "visualViewport", {
      configurable: true,
      value: viewport,
    });
    Object.assign(window, {
      __setTestVisualViewport(height: number, offsetTop: number) {
        viewport.height = height;
        viewport.offsetTop = offsetTop;
        viewport.dispatchEvent(new Event("resize"));
      },
    });
  }, geometry);
}

async function setTestSafeArea(page: Page) {
  await page.evaluate(() => {
    const root = document.documentElement;
    root.style.setProperty("--mobile-safe-area-top", "47px");
    root.style.setProperty("--mobile-safe-area-right", "13px");
    root.style.setProperty("--mobile-safe-area-bottom", "34px");
    root.style.setProperty("--mobile-safe-area-left", "11px");
  });
}

async function expectVisualViewportOverlay(page: Page, testId: string) {
  const geometry = await page.getByTestId(testId).evaluate((element) => {
    const overlayElement = element.closest<HTMLElement>(".viewport-overlay");
    if (!overlayElement) throw new Error("viewport overlay ownership missing");
    const overlay = overlayElement.getBoundingClientRect();
    const surface = element.matches("[data-scroll-owner='dialog']")
      ? (element as HTMLElement)
      : element.querySelector<HTMLElement>("[data-scroll-owner='dialog']");
    if (!surface) throw new Error("dialog surface ownership missing");
    const surfaceRect = surface.getBoundingClientRect();
    return {
      overlay: overlay.toJSON(),
      surface: surfaceRect.toJSON(),
      surfaceOverflowY: getComputedStyle(surface).overflowY,
    };
  });
  expect(geometry.overlay.top).toBeCloseTo(24, 0);
  expect(geometry.overlay.left).toBeCloseTo(8, 0);
  expect(geometry.overlay.width).toBeCloseTo(360, 0);
  expect(geometry.overlay.height).toBeCloseTo(500, 0);
  expect(geometry.surface.top).toBeGreaterThanOrEqual(24 + 47);
  expect(geometry.surface.left).toBeGreaterThanOrEqual(8 + 11);
  expect(geometry.surface.right).toBeLessThanOrEqual(8 + 360 - 13);
  expect(geometry.surface.bottom).toBeLessThanOrEqual(24 + 500 - 34);
  expect(geometry.surfaceOverflowY).toBe("auto");
}

test.describe("mobile viewport ownership", () => {
  test.beforeEach(({ page }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile viewport ownership guardrail",
    );
  });

  test("startup, fleet, and config use the route as their only page scroll owner", async ({
    page,
  }) => {
    for (const viewport of phoneViewports) {
      await page.setViewportSize(viewport);

      await gotoHarness(page, "bridge-unavailable");
      await expectShellClipsToRouteOwner(page);

      await gotoHarness(page);
      await expectShellClipsToRouteOwner(page);
      await expect(page.getByTestId("fleet-dashboard")).toHaveCSS(
        "overflow-y",
        "visible",
      );

      await openConfig(page);
      await expectShellClipsToRouteOwner(page);
      for (const selector of [
        ".config-page",
        ".config-workspace",
        ".config-tab-panel",
        ".config-editor",
      ]) {
        await expect(page.locator(selector).first()).toHaveCSS("overflow-y", "visible");
      }
    }
  });

  test("chat locks the route and delegates scrolling to its visible section", async ({
    page,
  }) => {
    await gotoHarness(page, "long-content");
    await openChat(page);

    const route = page.getByTestId("app-route-slot");
    expect(await route.getAttribute("data-scroll-owner")).toBeNull();
    await expect(route).toHaveCSS("overflow-y", "hidden");
    await expect(page.getByTestId("transcript-panel")).toHaveAttribute(
      "data-scroll-owner",
      "transcript",
    );
    expect(
      await page
        .locator("[data-scroll-owner]:visible")
        .evaluateAll((elements) =>
          elements.map((element) => element.dataset.scrollOwner),
        ),
    ).toEqual(["transcript"]);

    await openChatNavigation(page);
    await expect(page.locator(".conversation-list")).toHaveAttribute(
      "data-scroll-owner",
      "section-list",
    );
    await expect(page.locator(".session-group-list")).not.toHaveAttribute(
      "data-scroll-owner",
      /.+/,
    );
    await expect(page.locator(".session-group-list")).toHaveCSS(
      "overflow-y",
      "visible",
    );
    expect(
      await page
        .locator("[data-scroll-owner]:visible")
        .evaluateAll((elements) =>
          elements.map((element) => element.dataset.scrollOwner),
        ),
    ).toEqual(["section-list"]);
  });

  test("an error notice gets its own shell track without becoming a scroll owner", async ({
    page,
  }) => {
    await gotoHarness(page, "save-error");
    await openConfig(page);
    await openConfigTab(page, "behavior");
    await page
      .getByTestId("behavior-system-prompt")
      .fill("Trigger the rejected save viewport fixture.");
    await page.getByTestId("behavior-save").click();
    await expect(page.getByTestId("error-banner")).toBeVisible();

    const geometry = await page.evaluate(() => {
      const shell = document.querySelector<HTMLElement>(".app-shell");
      const notice = document.querySelector<HTMLElement>(".app-notice-slot");
      const route = document.querySelector<HTMLElement>(".app-route-slot");
      if (!shell || !notice || !route) throw new Error("notice geometry missing");
      return {
        notice: notice.getBoundingClientRect().toJSON(),
        route: route.getBoundingClientRect().toJSON(),
        shell: shell.getBoundingClientRect().toJSON(),
        noticeOwner: notice.dataset.scrollOwner ?? null,
        routeOwner: route.dataset.scrollOwner ?? null,
      };
    });
    expect(geometry.noticeOwner).toBeNull();
    expect(geometry.routeOwner).toBe("route");
    expect(geometry.route.top).toBeGreaterThanOrEqual(geometry.notice.bottom);
    expect(geometry.route.bottom).toBeLessThanOrEqual(geometry.shell.bottom);
  });

  test("dialogs consume one shared visual-viewport and safe-area primitive", async ({
    page,
  }) => {
    await installVisualViewport(page);
    await gotoHarness(page);
    await setTestSafeArea(page);

    await page.keyboard.press("ControlOrMeta+/");
    await expect(page.getByTestId("shortcuts-help")).toBeVisible();
    await expectVisualViewportOverlay(page, "shortcuts-help");
    await page.keyboard.press("Escape");

    await openConfig(page);
    await openConfigTab(page, "behavior");
    await page
      .getByTestId("behavior-system-prompt")
      .fill("Unsaved viewport ownership check");
    await page.getByTestId("config-tab-backends").click();
    await expect(page.getByTestId("confirm-dialog")).toBeVisible();
    await expectVisualViewportOverlay(page, "confirm-dialog");
  });

  test("context and sync popovers share visual-viewport containment", async ({
    page,
  }) => {
    await installVisualViewport(page);
    await gotoHarness(page, "long-content");
    await setTestSafeArea(page);
    await openChat(page);
    await page.locator("[data-testid='context-meter'] > summary").click();
    const context = page.locator(".context-meter-popover");
    await expect(context).toHaveAttribute("data-scroll-owner", "popover");
    const contextBounds = await context.evaluate((element) =>
      element.getBoundingClientRect().toJSON(),
    );
    expect(contextBounds.top).toBeGreaterThanOrEqual(24 + 47);
    expect(contextBounds.left).toBeGreaterThanOrEqual(8 + 11);
    expect(contextBounds.right).toBeLessThanOrEqual(8 + 360 - 13);
    expect(contextBounds.bottom).toBeLessThanOrEqual(24 + 500 - 34);

    await gotoHarness(page, "sync-stalled");
    await setTestSafeArea(page);
    await page.getByTestId("sync-health-summary").click();
    const details = page.getByTestId("sync-health-details");
    const bounds = await details.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return rect.toJSON();
    });
    expect(bounds.top).toBeGreaterThanOrEqual(24 + 47);
    expect(bounds.left).toBeGreaterThanOrEqual(8 + 11);
    expect(bounds.right).toBeLessThanOrEqual(8 + 360 - 13);
    expect(bounds.bottom).toBeLessThanOrEqual(24 + 500 - 34);
  });

  test("chat composer stays visible without turning the locked route into an owner", async ({
    page,
  }) => {
    await installVisualViewport(page);
    await gotoHarness(page, "long-content");
    await openChat(page);
    const composer = page.getByTestId("composer-input");
    await composer.focus();
    await page.evaluate(() => {
      (
        window as typeof window & {
          __setTestVisualViewport: (height: number, offsetTop: number) => void;
        }
      ).__setTestVisualViewport(300, 24);
    });

    await expect(page.locator(".app-shell")).toHaveCSS("height", "300px");
    const bounds = await composer.evaluate((element) =>
      element.getBoundingClientRect().toJSON(),
    );
    expect(bounds.top).toBeGreaterThanOrEqual(24);
    expect(bounds.bottom).toBeLessThanOrEqual(324);
    expect(
      await page.getByTestId("app-route-slot").getAttribute("data-scroll-owner"),
    ).toBeNull();
  });
});
