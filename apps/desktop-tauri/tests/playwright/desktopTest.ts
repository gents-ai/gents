import { expect, test as base, type Page, type TestInfo } from "@playwright/test";

export type HarnessScenario =
  | "default"
  | "empty-fleet"
  | "loading"
  | "bridge-unavailable"
  | "save-error"
  | "backend-health-error"
  | "long-content"
  | "active-turn"
  | "cascade-turn";

export const PEER_ID = "peer-bombadil-local";

type DesktopFixtures = {
  browserLogs: string[];
};

export const test = base.extend<DesktopFixtures>({
  browserLogs: async ({ page }, use, testInfo) => {
    const logs: string[] = [];
    page.on("console", (message) => {
      logs.push(`[console:${message.type()}] ${message.text()}`);
    });
    page.on("pageerror", (error) => {
      logs.push(`[pageerror] ${error.stack ?? error.message}`);
    });

    await use(logs);

    const unexpected = logs.filter(
      (line) => line.startsWith("[pageerror]") || line.startsWith("[console:error]"),
    );
    if (testInfo.status !== testInfo.expectedStatus || unexpected.length > 0) {
      await testInfo.attach("browser-console.log", {
        body: logs.join("\n") || "(no browser console output)",
        contentType: "text/plain",
      });
    }
    expect(unexpected).toEqual([]);
  },
});

export { expect };

export async function gotoHarness(page: Page, scenario: HarnessScenario = "default") {
  await page.goto(`/tests/ui-harness/harness.html?scenario=${scenario}`);
  await expect(page.locator(".app-shell")).toBeVisible();
  await expect(
    page
      .locator(
        [
          '[data-testid="fleet-dashboard"]',
          '[data-testid="fleet-empty"]',
          '[data-testid="transcript-panel"]',
          ".config-workspace",
          '[data-testid="error-banner"]',
        ].join(", "),
      )
      .first(),
  ).toBeVisible();
}

export async function openChat(page: Page) {
  await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
  await page.getByTestId(`fleet-chat-name-${PEER_ID}`).click();
  await expect(page.getByTestId("composer-input")).toBeVisible();
}

export async function openConfig(page: Page) {
  await expect(page.getByTestId("fleet-dashboard")).toBeVisible();
  await page.getByTestId(`fleet-chat-name-${PEER_ID}`).click();
  await expect(page.getByTestId("composer-input")).toBeVisible();
  await page.getByRole("button", { name: "Configure" }).click();
  await expect(page.locator(".config-workspace")).toBeVisible();
}

export async function openConfigTab(page: Page, tabId: string) {
  const tab = page.getByTestId(`config-tab-${tabId}`);
  await tab.click();
  await expect(tab).toHaveClass(/selected/);
}

export async function saveConfig(page: Page, testId: string) {
  await page.getByTestId(testId).click();
  await expect(page.locator(".config-editor").getByText("Saved")).toBeVisible();
}

export async function primarySurfaceCount(page: Page) {
  return page.evaluate(() => {
    const selectors = [
      '[data-testid="fleet-dashboard"]',
      '[data-testid="fleet-empty"]',
      '[data-testid="transcript-panel"]',
      ".config-workspace",
    ];
    return selectors.filter((selector) => document.querySelector(selector)).length;
  });
}

export async function enabledButtonsWithoutAccessibleNames(page: Page) {
  return page.evaluate(() => {
    return Array.from(document.querySelectorAll("button"))
      .filter((button) => !button.disabled)
      .map((button) => {
        const label =
          button.getAttribute("aria-label") ??
          button.getAttribute("title") ??
          button.textContent ??
          "";
        return {
          html: button.outerHTML,
          label: label.replace(/\s+/g, " ").trim(),
        };
      })
      .filter((button) => button.label.length === 0)
      .map((button) => button.html);
  });
}

export async function adjacentDuplicateTranscriptRows(page: Page) {
  return page
    .locator('[data-testid="transcript-panel"] .message-card')
    .evaluateAll((cards) => {
      const rows = cards.map((card) => {
        const roleText = card.querySelector(".message-role")?.textContent ?? "";
        const contentText = card.querySelector(".message-content")?.textContent ?? "";
        const role = roleText.replace(/\s+/g, " ").trim();
        const content = contentText.replace(/\s+/g, " ").trim();
        return { role, content };
      });
      const duplicates: string[] = [];
      for (let index = 1; index < rows.length; index += 1) {
        const previous = rows[index - 1];
        const current = rows[index];
        if (
          previous.role &&
          previous.content &&
          previous.role === current.role &&
          previous.content === current.content
        ) {
          duplicates.push(`${current.role}: ${current.content}`);
        }
      }
      return duplicates;
    });
}

export async function captureStableScreenshot(
  page: Page,
  testInfo: TestInfo,
  name: string,
) {
  const path = testInfo.outputPath(`${name}.png`);
  await page.screenshot({ fullPage: true, path });
  await testInfo.attach(`${name}.png`, {
    path,
    contentType: "image/png",
  });
}
