import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openChatNavigation,
  test,
} from "./desktopTest";

test.describe("mobile session sync fixture", () => {
  test("opens a pre-existing session and hydrates history without blocking local rows", async ({
    page,
  }) => {
    await gotoHarness(page, "session-hydration");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "syncing",
    );
    await openChat(page);
    await openChatNavigation(page);
    await page.getByTestId("conversation-session-remote").click();
    await expect(
      page.getByTestId("transcript-panel").getByText("hello from desktop"),
    ).toBeVisible();
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "requested",
    );

    await page.evaluate(() => window.__GENTS_SESSION_SYNC__?.progress(2, 4));
    await expect(page.getByTestId("session-hydration-status")).toHaveText(/2 of 4/);
    await expect(
      page
        .getByTestId("transcript-panel")
        .getByText("history arrived from the desktop"),
    ).toBeVisible();

    if ((page.viewportSize()?.width ?? 761) <= 760) {
      const geometry = await page.evaluate(() => {
        const shell = document.querySelector<HTMLElement>(".app-shell");
        const header = document.querySelector<HTMLElement>(".chat-header");
        const hydration = document.querySelector<HTMLElement>(
          "[data-testid=session-hydration-status]",
        );
        const transcript = document.querySelector<HTMLElement>(
          "[data-testid=transcript-panel]",
        );
        const composer = document.querySelector<HTMLElement>(".composer-panel");
        if (!shell || !header || !hydration || !transcript || !composer) {
          throw new Error("mobile hydration geometry missing");
        }
        return {
          shell: shell.getBoundingClientRect().toJSON(),
          header: header.getBoundingClientRect().toJSON(),
          hydration: hydration.getBoundingClientRect().toJSON(),
          transcript: transcript.getBoundingClientRect().toJSON(),
          composer: composer.getBoundingClientRect().toJSON(),
          headerScrollWidth: header.scrollWidth,
          headerClientWidth: header.clientWidth,
        };
      });

      expect(geometry.headerScrollWidth).toBeLessThanOrEqual(
        geometry.headerClientWidth,
      );
      expect(geometry.header.left).toBeGreaterThanOrEqual(geometry.shell.left);
      expect(geometry.header.right).toBeLessThanOrEqual(geometry.shell.right);
      expect(geometry.hydration.height).toBeLessThanOrEqual(56);
      expect(geometry.transcript.height).toBeGreaterThan(geometry.hydration.height * 2);
      expect(geometry.transcript.top).toBeGreaterThanOrEqual(geometry.hydration.bottom);
      expect(geometry.transcript.bottom).toBeLessThanOrEqual(geometry.composer.top);
      expect(geometry.composer.bottom).toBeLessThanOrEqual(geometry.shell.bottom);
    }

    await page.evaluate(() => window.__GENTS_SESSION_SYNC__?.complete());
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "complete",
    );
    await expect(page.getByTestId("session-hydration-status")).toContainText("4 of 4");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "healthy",
    );
  });

  test("failed hydration retries through the existing session snapshot path", async ({
    page,
  }) => {
    await gotoHarness(page, "session-hydration");
    await openChat(page);
    await openChatNavigation(page);
    await page.getByTestId("conversation-session-remote").click();
    await page.evaluate(() => window.__GENTS_SESSION_SYNC__?.fail());
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "failed",
    );
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "failed",
    );
    await page.getByTestId("session-hydration-retry").click();
    await expect(page.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "requested",
    );
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "syncing",
    );
  });

  test("global indicator distinguishes offline, stalled, and failed", async ({
    page,
  }) => {
    await gotoHarness(page, "sync-offline");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "offline",
    );
    await expect(page.getByTestId("sync-health-summary")).toContainText(
      "Offline since",
    );

    await gotoHarness(page, "sync-stalled");
    await page.evaluate(() => {
      const root = document.documentElement;
      root.style.setProperty("--mobile-safe-area-top", "47px");
      root.style.setProperty("--mobile-safe-area-right", "13px");
      root.style.setProperty("--mobile-safe-area-bottom", "34px");
      root.style.setProperty("--mobile-safe-area-left", "11px");
    });
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "stalled",
    );
    await page.getByTestId("sync-health-summary").click();
    const details = page.getByTestId("sync-health-details");
    await expect(details).toContainText("RpcTimeout");
    const bounds = await details.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return {
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
        left: rect.left,
        viewportHeight: window.innerHeight,
        viewportWidth: window.innerWidth,
        computedMaxHeight: Number.parseFloat(getComputedStyle(element).maxHeight),
      };
    });
    if (bounds.viewportWidth <= 760) {
      expect(bounds.computedMaxHeight).toBeCloseTo(
        bounds.viewportHeight - 47 - 34 - 80,
        0,
      );
      expect(bounds.top).toBeGreaterThanOrEqual(47 + 64);
      expect(bounds.left).toBeGreaterThanOrEqual(11 + 8);
      expect(bounds.right).toBeLessThanOrEqual(bounds.viewportWidth - 13 - 8);
      expect(bounds.bottom).toBeLessThanOrEqual(bounds.viewportHeight - 34);
    }
    await expectNoPageHorizontalOverflow(page);

    await gotoHarness(page, "sync-failed");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "failed",
    );
  });

  test("offline recovers through the existing reconnect control", async ({ page }) => {
    await gotoHarness(page, "sync-offline");
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "offline",
    );
    await page.getByTestId("fleet-repair-p2p").click();
    await expect(page.getByTestId("sync-health-indicator")).toHaveAttribute(
      "data-sync-state",
      "healthy",
    );
  });
});
