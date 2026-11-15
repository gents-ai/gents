import {
  expect,
  test as base,
  type Locator,
  type Page,
  type TestInfo,
} from "@playwright/test";

import type { DesktopUiHarnessScenario } from "../ui-harness/desktopHarness";
import { OWN_VIEWPORT, PROJECT_SIZE_SENTINEL } from "./ownViewport";

export type HarnessScenario = DesktopUiHarnessScenario;

export const PEER_ID = "peer-bombadil-local";

type DesktopFixtures = {
  browserLogs: string[];
  ownViewportGuard: void;
};

/* a tag is read from the test's details or from its title */
function ownViewport(testInfo: TestInfo) {
  return (
    testInfo.tags.includes(OWN_VIEWPORT) ||
    testInfo.titlePath.some((title) => title.split(/\s+/).includes(OWN_VIEWPORT))
  );
}

function isSentinel(size: { width: number; height: number } | null) {
  return (
    size?.width === PROJECT_SIZE_SENTINEL.width &&
    size?.height === PROJECT_SIZE_SENTINEL.height
  );
}

export const test = base.extend<DesktopFixtures>({
  /* a spec's own test.use({ viewport }) replaces this fixture, so only a
     tagged test that sets no size of its own starts at the sentinel */
  viewport: async ({ viewport }, use, testInfo) => {
    await use(ownViewport(testInfo) ? PROJECT_SIZE_SENTINEL : viewport);
  },
  ownViewportGuard: [
    async ({ page }, use, testInfo) => {
      if (!ownViewport(testInfo)) {
        await use();
        return;
      }
      const size = page.viewportSize.bind(page);
      const unsized: string[] = [];
      const onNavigation = (frame: ReturnType<Page["mainFrame"]>) => {
        if (frame === page.mainFrame() && isSentinel(size())) {
          unsized.push(frame.url());
        }
      };
      page.on("framenavigated", onNavigation);
      page.viewportSize = () => {
        const current = size();
        if (isSentinel(current)) {
          throw new Error(
            `${OWN_VIEWPORT} test read the window size before setting its own; untag it so it runs on every project`,
          );
        }
        return current;
      };

      await use();

      page.off("framenavigated", onNavigation);
      expect(
        unsized,
        `${OWN_VIEWPORT} test navigated before setting its own window size; untag it so it runs on every project`,
      ).toEqual([]);
    },
    { auto: true },
  ],
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

export { expect, OWN_VIEWPORT };
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

/** Resolves once the page has drawn `count` more frames. Headless WebKit draws
    about 20 a second where Chromium draws 60 or more, and fewer still under
    load, so work measured per frame is paced in frames, not milliseconds. */
export async function framesDrawn(page: Page, count: number) {
  await page.evaluate(
    (count) =>
      new Promise<void>((resolve) => {
        let left = count;
        const next = () => (--left > 0 ? requestAnimationFrame(next) : resolve());
        requestAnimationFrame(next);
      }),
    count,
  );
}

/** Resolves once `scroller` has kept its position across two drawn frames. A
    wheel returns before the scroll it starts has landed, and an older page
    landing above moves the position again. */
export async function scrollAtRest(scroller: Locator) {
  await scroller.evaluate(async (element) => {
    const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));
    let top: number;
    do {
      top = element.scrollTop;
      await frame();
      await frame();
    } while (element.scrollTop !== top);
  });
}

/* an init script runs in every frame of the page, and a plugin frame's origin
   is opaque: its storage throws, so the seed stays in the top window */
export async function seedLocalStorage(page: Page, entries: Record<string, string>) {
  await page.addInitScript((seed: Record<string, string>) => {
    if (window !== window.top) return;
    for (const [key, value] of Object.entries(seed)) localStorage.setItem(key, value);
  }, entries);
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
    const newChat = page.getByRole("button", { name: "New", exact: true });
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
  await page.getByRole("link", { name: "Active node", exact: true }).click();
  await expect(page.getByTestId("agent-screen")).toBeVisible();
}

export async function openConfigSection(page: Page, name: RegExp) {
  const sectionPicker = page.getByRole("combobox", { name: "Section" });
  if (await sectionPicker.isVisible()) {
    await sectionPicker.click();
    await page.getByRole("option", { name }).click();
    return;
  }
  await page.mouse.move(page.viewportSize()!.width - 30, 100);
  /* the section list's link, not a summary card leading to the same place */
  await page
    .getByTestId("agent-screen")
    .locator('[data-slot="sidebar-nav"]')
    .getByRole("link", { name })
    .click();
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
  await page.evaluate(async () => {
    const styleId = "playwright-stable-screenshot-style";
    if (!document.getElementById(styleId)) {
      const style = document.createElement("style");
      style.id = styleId;
      style.textContent = `
        *, *::before, *::after {
          animation: none !important;
          caret-color: transparent !important;
          transition: none !important;
        }
      `;
      document.head.append(style);
    }
    await document.fonts.ready;
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  });
  const path = testInfo.outputPath(`${name}.png`);
  await page.screenshot({ fullPage: true, path });
  const attachmentName = `${name}.png`;
  await testInfo.attach(attachmentName, {
    path,
    contentType: "image/png",
  });
  return { attachmentName, path };
}
