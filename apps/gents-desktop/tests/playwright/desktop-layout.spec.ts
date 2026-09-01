import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openChatNavigation,
  openConfig,
  openConfigTab,
  PEER_ID,
  test,
} from "./desktopTest";

const chatScenarios = [
  "default",
  "long-content",
  "active-turn",
  "cascade-turn",
  "coding",
  "session-hydration",
  "backend-unavailable",
  "tool-hold",
] as const;

const shellScenarios = [
  "empty-fleet",
  "loading",
  "bridge-unavailable",
  "save-error",
  "backend-health-error",
  "sync-offline",
  "sync-stalled",
  "sync-failed",
] as const;

test.describe("desktop responsive layout guardrails", () => {
  for (const scenario of chatScenarios) {
    test(`${scenario} has no page-level horizontal overflow`, async ({ page }) => {
      await gotoHarness(page, scenario);
      await openChat(page);
      await expectNoPageHorizontalOverflow(page);
    });
  }

  for (const scenario of shellScenarios) {
    test(`${scenario} has no page-level horizontal overflow`, async ({ page }) => {
      await gotoHarness(page, scenario);
      await expectNoPageHorizontalOverflow(page);
    });
  }

  test("config tabs remain reachable without widening the page", async ({ page }) => {
    await gotoHarness(page);
    await openConfig(page);

    for (const tabId of [
      "agent",
      "behavior",
      "backends",
      "profiles",
      "toolSelections",
      "metaTools",
      "tasks",
      "timerTriggers",
      "eventTriggers",
    ]) {
      await openConfigTab(page, tabId);
      await expect(page.locator(".config-editor").first()).toBeVisible();
      await expectNoPageHorizontalOverflow(page);
    }
  });

  test("adversarial mailbox content stays inside the phone sidebar", async ({
    page,
  }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile viewport guardrail",
    );
    await gotoHarness(page, "mailbox-overflow");
    await openChat(page);
    await openChatNavigation(page);
    await page.getByTestId("agent-tab-mailbox").click();
    await expect(page.locator(".mailbox-item")).toBeVisible();
    await expectNoPageHorizontalOverflow(page);
  });

  test("multiple approval holds preserve the composer and final actions", async ({
    page,
  }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile viewport guardrail",
    );
    await gotoHarness(page, "tool-hold");
    await openChat(page);
    const finalAction = page.getByTestId("hold-approve-hold-mobile-6");
    await finalAction.scrollIntoViewIfNeeded();
    await expect(finalAction).toBeVisible();
    const contained = await page.evaluate(() => {
      const chat = document.querySelector<HTMLElement>(".chat-main");
      const composer = document.querySelector<HTMLElement>(".composer-panel");
      const finalAction = document.querySelector<HTMLElement>(
        '[data-testid="hold-approve-hold-mobile-6"]',
      );
      if (!chat || !composer || !finalAction) throw new Error("holds geometry missing");
      const chatRect = chat.getBoundingClientRect();
      const composerRect = composer.getBoundingClientRect();
      const actionRect = finalAction.getBoundingClientRect();
      return {
        composer:
          composerRect.top >= chatRect.top && composerRect.bottom <= chatRect.bottom,
        finalAction:
          actionRect.top >= chatRect.top && actionRect.bottom <= chatRect.bottom,
      };
    });
    expect(contained).toEqual({ composer: true, finalAction: true });
  });

  test("phone chat uses one full-screen pane at a time", async ({ page }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile viewport guardrail",
    );

    await gotoHarness(page);
    await openChat(page);
    await expect(page.locator(".chat-column")).toBeVisible();
    await expect(page.locator(".sidebar")).toBeHidden();

    await openChatNavigation(page);
    await expect(page.locator(".sidebar")).toBeVisible();
    await expect(page.locator(".chat-column")).toBeHidden();

    for (const tab of ["sessions", "mailbox", "behaviors"]) {
      await page.getByTestId(`agent-tab-${tab}`).click();
      const geometry = await page.evaluate(() => {
        const sidebar = document.querySelector<HTMLElement>(".sidebar");
        const tabs = document.querySelector<HTMLElement>(".agent-section-tabs");
        const section = sidebar?.lastElementChild as HTMLElement | null;
        if (!sidebar || !tabs || !section) throw new Error("sidebar geometry missing");
        return {
          sidebar: sidebar.getBoundingClientRect().toJSON(),
          tabs: tabs.getBoundingClientRect().toJSON(),
          section: section.getBoundingClientRect().toJSON(),
        };
      });
      expect(geometry.tabs.height).toBeLessThanOrEqual(56);
      expect(geometry.section.top).toBeGreaterThanOrEqual(geometry.tabs.bottom);
      expect(geometry.section.bottom).toBeLessThanOrEqual(geometry.sidebar.bottom);
      await expectNoPageHorizontalOverflow(page);
    }

    await page.getByTestId("agent-tab-sessions").click();
    await page.getByTestId("conversation-session-intro").click();
    await expect(page.locator(".chat-column")).toBeVisible();
    await expect(page.locator(".sidebar")).toBeHidden();
  });

  test("fleet deployment navigation stays reachable at any width", async ({ page }) => {
    await gotoHarness(page);
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    const deploymentRow = page.getByTestId(`fleet-row-${PEER_ID}`);
    await expect(deploymentRow).toBeVisible();
    await deploymentRow.click();
    await page.getByTestId("agent-actions").click();
    const configureButton = page.getByRole("button", { name: "Configure" });
    await expect(configureButton).toBeVisible();

    await expectNoPageHorizontalOverflow(page);

    await configureButton.click();
    await expect(page.locator(".config-workspace")).toBeVisible();
    await expectNoPageHorizontalOverflow(page);
  });

  test("empty-fleet remote connection submit stays reachable on mobile", async ({
    page,
  }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile viewport guardrail",
    );

    await gotoHarness(page, "empty-fleet");
    await page
      .getByTestId("fleet-remote-disclosure")
      .locator(":scope > summary")
      .click();

    const statusForm = page.getByTestId("fleet-status-form");
    await expect(statusForm).toBeVisible();
    await expect(page.locator(".fleet-manual-disclosure")).toHaveCount(0);
    await expect(page.locator(".fleet-alternative-disclosure")).toHaveCount(0);

    await page.getByTestId("fleet-add-server-address").fill("http://studio-1:9191");

    const submit = page.getByTestId("fleet-fetch-status");
    await expect(submit).toBeAttached();
    await page.getByTestId("app-route-slot").evaluate((owner) => {
      owner.scrollTop = owner.scrollHeight;
    });
    await expect(submit).toBeVisible();
    await submit.click({ trial: true });

    const submitRect = await submit.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return { bottom: rect.bottom, left: rect.left, right: rect.right, top: rect.top };
    });
    const viewport = page.viewportSize();
    expect(viewport).not.toBeNull();
    expect(submitRect.top).toBeGreaterThanOrEqual(0);
    expect(submitRect.left).toBeGreaterThanOrEqual(0);
    expect(submitRect.right).toBeLessThanOrEqual(viewport!.width);
    expect(submitRect.bottom).toBeLessThanOrEqual(viewport!.height);
    await expectNoPageHorizontalOverflow(page);
  });

  test("keyboard viewport resize reveals the focused enrollment address", async ({
    page,
  }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile viewport guardrail",
    );
    await page.addInitScript(() => {
      const viewport = new EventTarget() as EventTarget & {
        height: number;
        width: number;
        offsetTop: number;
        offsetLeft: number;
      };
      Object.assign(viewport, {
        height: window.innerHeight,
        width: window.innerWidth,
        offsetTop: 0,
        offsetLeft: 0,
      });
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
    });

    await gotoHarness(page, "empty-fleet");
    await page
      .getByTestId("fleet-remote-disclosure")
      .locator(":scope > summary")
      .click();
    const address = page.getByTestId("fleet-add-server-address");
    await address.focus();
    const route = page.getByTestId("app-route-slot");
    await route.evaluate((owner) => {
      owner.scrollTop = 0;
    });
    await page.evaluate(() => {
      (
        window as typeof window & {
          __setTestVisualViewport: (height: number, offsetTop: number) => void;
        }
      ).__setTestVisualViewport(360, 24);
    });

    await expect
      .poll(() => route.evaluate((owner) => owner.scrollTop))
      .toBeGreaterThan(0);
    await expect(page.locator(".app-shell")).toHaveCSS("height", "360px");
    const addressRect = await address.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return { bottom: rect.bottom, top: rect.top };
    });
    expect(addressRect.top).toBeGreaterThanOrEqual(24);
    expect(addressRect.bottom).toBeLessThanOrEqual(384);
    expect(await page.locator(".app-shell").evaluate((shell) => shell.scrollTop)).toBe(
      0,
    );
    expect(
      await page.getByTestId("fleet-empty").evaluate((fleet) => fleet.scrollTop),
    ).toBe(0);
  });

  test("populated-fleet status discovery stays reachable on mobile", async ({
    page,
  }) => {
    test.skip(
      (page.viewportSize()?.width ?? Number.POSITIVE_INFINITY) > 760,
      "mobile viewport guardrail",
    );

    await gotoHarness(page);
    await page.getByRole("button", { name: "Add Agent", exact: true }).click();

    const address = page.getByTestId("fleet-add-server-address");
    const fetchStatus = page.getByTestId("fleet-fetch-status");
    const route = page.getByTestId("app-route-slot");
    await route.evaluate((owner) => {
      owner.scrollTop = owner.scrollHeight;
    });
    await expect(address).toBeVisible();
    await address.fill("http://studio-1:9191");

    await route.evaluate((owner) => {
      owner.scrollTop = owner.scrollHeight;
    });
    await expect(fetchStatus).toBeVisible();
    await fetchStatus.click({ trial: true });

    const fetchStatusRect = await fetchStatus.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return { bottom: rect.bottom, left: rect.left, right: rect.right, top: rect.top };
    });
    const viewport = page.viewportSize();
    expect(viewport).not.toBeNull();
    expect(fetchStatusRect.top).toBeGreaterThanOrEqual(0);
    expect(fetchStatusRect.left).toBeGreaterThanOrEqual(0);
    expect(fetchStatusRect.right).toBeLessThanOrEqual(viewport!.width);
    expect(fetchStatusRect.bottom).toBeLessThanOrEqual(viewport!.height);
    await expectNoPageHorizontalOverflow(page);
  });
});
