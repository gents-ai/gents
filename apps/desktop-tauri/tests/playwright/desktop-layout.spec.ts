import {
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openConfig,
  openConfigTab,
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
});
