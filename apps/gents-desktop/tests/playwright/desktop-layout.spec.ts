import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openConfig,
  openConfigTab,
  PEER_ID,
  test,
} from "./desktopTest";

const scenarios = [
  "default",
  "empty-fleet",
  "loading",
  "long-content",
  "operations-rich",
] as const;

test.describe("desktop responsive layout guardrails", () => {
  for (const scenario of scenarios) {
    test(`${scenario} has no page-level horizontal overflow`, async ({ page }) => {
      await gotoHarness(page, scenario);
      if (scenario !== "empty-fleet" && scenario !== "loading") {
        await openChat(page);
      }
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

  test("opened operations drawer stays inside the viewport", async ({ page }) => {
    await gotoHarness(page, "operations-rich");
    await openChat(page);
    await page.getByRole("button", { name: /open operations drawer/i }).click();

    for (const tab of [/Background/, /Lineage/, /Backends/, /MCP health/]) {
      await page.getByRole("tab", { name: tab }).click();
      await expect(
        page.getByRole("complementary", { name: "Operations" }),
      ).toBeVisible();
      await expectNoPageHorizontalOverflow(page);
    }
  });

  test("fleet row action buttons stay reachable at any width", async ({ page }) => {
    await gotoHarness(page); // default scenario renders peer-bombadil-local
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
    await expect(page.getByTestId(`fleet-row-${PEER_ID}`)).toBeVisible();

    const chatButton = page.getByTestId(`fleet-chat-${PEER_ID}`);
    const configButton = page.getByTestId(`fleet-config-${PEER_ID}`);

    // Both action buttons exist even when scrolled off-screen at 390px.
    await expect(chatButton).toBeAttached();
    await expect(configButton).toBeAttached();

    // Reachability mechanism: the actions cell lives inside a horizontally
    // scrollable container (.fleet-table-wrap { overflow: auto }) — NOT an
    // overflow:hidden clip that would make the buttons permanently unreachable.
    const scrollableAncestor = await configButton.evaluate((el) => {
      let node: HTMLElement | null = el.closest("td");
      while (node) {
        const overflowX = getComputedStyle(node).overflowX;
        if (overflowX === "auto" || overflowX === "scroll") {
          return { className: node.className, overflowX };
        }
        node = node.parentElement;
      }
      return null;
    });
    expect(scrollableAncestor?.className ?? "").toContain("fleet-table-wrap");

    // A real user can reach every action button: scroll it into view, then it is
    // visible and hit-testable (trial click proves it is not clipped/covered).
    for (const button of [chatButton, configButton]) {
      await button.scrollIntoViewIfNeeded();
      await expect(button).toBeVisible();
      await button.click({ trial: true });
    }

    // Reaching the actions must never introduce page-level horizontal overflow.
    await expectNoPageHorizontalOverflow(page);

    // The config action actually works once reached (reach + activate contract).
    await configButton.click();
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
    await page.getByTestId("fleet-remote-disclosure").locator("summary").click();
    await page.getByTestId("fleet-add-server-address").fill("http://studio-1:9191");

    const submit = page.getByTestId("fleet-add-submit");
    await expect(submit).toBeAttached();
    await submit.scrollIntoViewIfNeeded();
    await expect(submit).toBeVisible();
    await submit.click({ trial: true });

    const shellScrollTop = await page
      .locator(".app-shell")
      .evaluate((element) => element.scrollTop);
    expect(shellScrollTop).toBeGreaterThan(0);
    await expectNoPageHorizontalOverflow(page);
  });
});
