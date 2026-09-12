import { expect, test as base, type Page, type TestInfo } from "@playwright/test";

export type HarnessScenario =
  | "default"
  | "empty-fleet"
  | "loading"
  | "bridge-unavailable"
  | "save-error"
  | "backend-health-error"
  | "backend-unavailable"
  | "mailbox-overflow"
  | "long-content"
  | "active-turn"
  | "cascade-turn"
  | "coding"
  | "session-hydration"
  | "sync-offline"
  | "sync-failed";

export const PEER_ID = "peer-bombadil-local";

type DesktopFixtures = {
  browserLogs: string[];
};

export const test = base.extend<DesktopFixtures>({
  browserLogs: [
    async ({ page }, use, testInfo) => {
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
    { auto: true },
  ],
});

export { expect };
export type { Page, TestInfo };

const KIT_SURFACE = [
  '[data-testid="app-shell"]',
  '[data-testid="setup-screen"]',
  '[data-testid="startup-screen"]',
].join(", ");

export async function gotoHarness(page: Page, scenario: HarnessScenario = "default") {
  await page.goto(`/tests/ui-harness/harness.html?scenario=${scenario}`);
  await expect(page.locator(KIT_SURFACE).first()).toBeVisible();
}

export async function gotoLiveHarness(page: Page, bridgeUrl?: string) {
  const params = new URLSearchParams({ backend: "live" });
  if (bridgeUrl) {
    params.set("bridgeUrl", bridgeUrl);
  }
  await page.goto(`/tests/ui-harness/harness.html?${params.toString()}`);
  await expect(page.locator(KIT_SURFACE).first()).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute(
    "data-desktop-ui-harness-backend",
    "live",
  );
}

export function composer(page: Page) {
  return page.getByRole("textbox", { name: "Message" });
}

export function sendButton(page: Page) {
  return page.getByRole("button", { name: "Send" });
}

export async function openChat(page: Page) {
  await expect(page.getByTestId("app-shell")).toBeVisible();
  if (await page.getByTestId("sessions-screen").count()) {
    const newChat = page.getByRole("button", { name: "New" });
    if (await newChat.count()) {
      await newChat.click();
    } else {
      await page.getByRole("button", { name: "Menu" }).click();
      await page.getByRole("link", { name: "New session" }).click();
    }
  }
  await expect(page.getByTestId("session-screen")).toBeVisible();
  await expect(composer(page)).toBeVisible();
}

export async function openConfig(page: Page) {
  await expect(page.getByTestId("app-shell")).toBeVisible();
  const configLink = page.getByRole("link", { name: /configuration/i });
  if (await configLink.first().isVisible()) {
    await configLink.first().click();
  } else {
    await page.getByRole("button", { name: "Menu" }).click();
    await page
      .getByRole("link", { name: /configuration|Configure/i })
      .first()
      .click();
  }
  await expect(page.getByTestId("agent-screen")).toBeVisible();
}

export async function primarySurfaceCount(page: Page) {
  return page.evaluate(() => {
    const selectors = [
      '[data-testid="app-shell"]',
      '[data-testid="setup-screen"]',
      '[data-testid="startup-screen"]',
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

export async function expectNoPageHorizontalOverflow(page: Page) {
  const overflow = await page.evaluate(() => {
    const documentWidth = Math.max(
      document.documentElement.scrollWidth,
      document.body?.scrollWidth ?? 0,
    );
    return {
      documentWidth,
      viewportWidth: window.innerWidth,
      offenders: Array.from(
        document.querySelectorAll(
          [
            '[data-testid="app-shell"]',
            '[data-testid="setup-screen"]',
            '[data-testid="startup-screen"]',
            '[data-testid="sessions-screen"]',
            '[data-testid="session-screen"]',
            '[data-testid="agents-screen"]',
            '[data-testid="agent-screen"]',
            '[data-testid="mailbox-screen"]',
            '[data-testid="composer"]',
          ].join(", "),
        ),
      )
        .filter((element) => !element.closest("details:not([open])"))
        .map((element) => {
          const htmlElement = element as HTMLElement;
          const style = window.getComputedStyle(htmlElement);
          return {
            selector:
              htmlElement.getAttribute("data-testid") ??
              htmlElement.className.toString() ??
              htmlElement.tagName,
            clientWidth: htmlElement.clientWidth,
            scrollWidth: htmlElement.scrollWidth,
            overflowX: style.overflowX,
            bounds: htmlElement.getBoundingClientRect().toJSON(),
          };
        })
        .filter((entry) => {
          const scrollDelta = entry.scrollWidth - entry.clientWidth;
          const escapesViewport =
            entry.bounds.width > 0 &&
            (entry.bounds.left < -2 || entry.bounds.right > window.innerWidth + 2);
          return escapesViewport || (scrollDelta > 2 && entry.overflowX === "visible");
        }),
    };
  });

  expect(overflow.documentWidth).toBeLessThanOrEqual(overflow.viewportWidth + 2);
  expect(overflow.offenders).toEqual([]);
}

export async function captureStableScreenshot(
  page: Page,
  testInfo: TestInfo,
  name: string,
): Promise<{ attachmentName: string; path: string }> {
  const path = testInfo.outputPath(`${name}.png`);
  await page.screenshot({ fullPage: true, path });
  const attachmentName = `${name}.png`;
  await testInfo.attach(attachmentName, {
    path,
    contentType: "image/png",
  });
  return { attachmentName, path };
}
