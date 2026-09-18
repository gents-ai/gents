import {
  composer,
  enabledButtonsWithoutAccessibleNames,
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openConfig,
  openConfigSection,
  primarySurfaceCount,
  sendButton,
  test,
  type Page,
} from "./desktopTest";

type DialogSample = {
  count: number;
  dialogs: Record<string, string | null>[];
};

async function startDialogObservation(page: Page) {
  await page.evaluate(() => {
    const samples: DialogSample[] = [];
    const record = () =>
      samples.push({
        count: document.querySelectorAll('[role="dialog"]').length,
        dialogs: Array.from(document.querySelectorAll('[role="dialog"]')).map(
          (dialog) => ({
            slot: dialog.getAttribute("data-slot"),
            open: dialog.getAttribute("data-open"),
            closed: dialog.getAttribute("data-closed"),
            label: dialog.getAttribute("aria-label"),
          }),
        ),
      });
    record();
    const observer = new MutationObserver(record);
    observer.observe(document.documentElement, {
      attributes: true,
      childList: true,
      subtree: true,
    });
    Object.assign(window, { __gentsDialogObservation: { observer, samples } });
  });
}

async function finishDialogObservation(page: Page) {
  return page.evaluate(() => {
    const transition = (
      window as typeof window & {
        __gentsDialogObservation: {
          observer: MutationObserver;
          samples: DialogSample[];
        };
      }
    ).__gentsDialogObservation;
    transition.observer.disconnect();
    const current = document.querySelectorAll('[role="dialog"]').length;
    return {
      current,
      max: Math.max(...transition.samples.map((sample) => sample.count), current),
      overlapping: transition.samples.filter((sample) => sample.count > 1),
    };
  });
}

async function expectAppShellAtViewport(
  page: Page,
  viewport: { width: number; height: number },
) {
  await expect
    .poll(() =>
      page.getByTestId("app-shell").evaluate((element) => {
        const bounds = element.getBoundingClientRect();
        return {
          viewportWidth: window.innerWidth,
          viewportHeight: window.innerHeight,
          left: bounds.left,
          top: bounds.top,
          right: bounds.right,
          bottom: bounds.bottom,
        };
      }),
    )
    .toEqual({
      viewportWidth: viewport.width,
      viewportHeight: viewport.height,
      left: 0,
      top: 0,
      right: viewport.width,
      bottom: viewport.height,
    });
}

test.describe("kit shell", () => {
  test("keyboard focus opens the rail flyout", async ({ page }) => {
    test.skip(test.info().project.name === "chromium-narrow", "No desktop rail");
    await gotoHarness(page);
    await page.getByRole("link", { name: /configuration/i }).focus();
    await page.keyboard.press("Tab");
    await expect(page.getByRole("link", { name: /Configure/ })).toBeVisible();
  });
  test("rail pointer focus does not cover a link before mouseup", async ({ page }) => {
    test.skip(test.info().project.name === "chromium-narrow", "No desktop rail");
    await gotoHarness(page);
    const link = page.getByRole("link", { name: /configuration/i });
    const bounds = await link.boundingBox();
    expect(bounds).not.toBeNull();
    await page.mouse.move(
      bounds!.x + bounds!.width / 2,
      bounds!.y + bounds!.height / 2,
    );
    await page.mouse.down();
    // Let pointer-induced focus settle before releasing, as under browser load.
    await page.waitForTimeout(350);
    await page.mouse.up();
    await expect(page.getByTestId("agent-screen")).toBeVisible();
  });
  test("mac content viewport has no overlay chrome inset and stays bounded on resize", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openChat(page);
    await page.evaluate(() => {
      document.documentElement.dataset.shell = "mac";
    });
    for (const viewport of [
      { width: 1180, height: 720 },
      { width: 1480, height: 868 },
      { width: 1180, height: 696 },
    ]) {
      await page.setViewportSize(viewport);
      await expectAppShellAtViewport(page, viewport);
      await expectNoPageHorizontalOverflow(page);
      await expect(composer(page)).toBeVisible();
      const bounds = await page.evaluate(() => {
        const frame = document
          .querySelector('[data-testid="app-shell"]')!
          .getBoundingClientRect();
        const header = document.querySelector(".app-titlebar")!;
        const editor = document.querySelector("textarea")!.getBoundingClientRect();
        return {
          top: frame.top,
          bottom: frame.bottom,
          headerTop: header.getBoundingClientRect().top,
          headerInset: getComputedStyle(header).paddingLeft,
          editorBottom: editor.bottom,
          height: window.innerHeight,
        };
      });
      expect(bounds.top).toBe(0);
      expect(bounds.headerTop).toBe(0);
      expect(bounds.headerInset).toBe("16px");
      expect(bounds.bottom).toBeLessThanOrEqual(bounds.height);
      expect(bounds.editorBottom).toBeLessThanOrEqual(bounds.height);
    }
  });
  test("default harness lands on sessions with one primary surface", async ({
    page,
  }) => {
    await gotoHarness(page);
    await expect(page.getByTestId("app-shell")).toBeVisible();
    await expect(page.getByTestId("sessions-screen")).toBeVisible();
    await expect(primarySurfaceCount(page)).resolves.toBe(1);
    await expect(enabledButtonsWithoutAccessibleNames(page)).resolves.toEqual([]);
    await expectNoPageHorizontalOverflow(page);
  });

  test("new session opens the composer", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await expect(composer(page)).toBeEditable();
  });

  test("session filters stay visible and can be reset", async ({ page }) => {
    await gotoHarness(page);
    await expect(page.getByLabel("Session filters")).toBeVisible();
    await expect(page.getByLabel("Filter by behaviour")).toContainText(
      "All behaviours",
    );
    await expect(page.getByLabel("Filter by state")).toContainText("Any state");
    await expect(page.getByLabel("Filter by source")).toContainText("Any source");

    await page.getByLabel("Filter by behaviour").click();
    await page.getByRole("option", { name: "Ops" }).click();
    await expect(page.getByText("Showing 0 of 1")).toBeVisible();
    await expect(page.getByRole("button", { name: "Clear filters" })).toBeVisible();

    await page.getByRole("button", { name: "Clear filters" }).click();
    await expect(page.getByText("Showing 1 of 1")).toBeVisible();
    await expect(
      page.getByRole("link", { name: /introduction-and-greetings/ }),
    ).toBeVisible();
  });

  test("agents and configuration are reachable", async ({ page }) => {
    await gotoHarness(page);
    await page.getByLabel("breadcrumb").getByRole("link", { name: "Agents" }).click();
    await expect(page.getByTestId("agents-screen")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
    await expect(page.getByText("Network", { exact: true })).toHaveCount(0);

    await openConfig(page);
    await expect(page.getByText("Agent details")).toBeVisible();
  });

  test("closing sync diagnostics does not stack with the add agent dialog", async ({
    page,
  }) => {
    await gotoHarness(page);
    await page.getByLabel("breadcrumb").getByRole("link", { name: "Agents" }).click();
    const sync = page.getByRole("button", { name: /Sync healthy/ });
    const syncDialog = page.getByRole("dialog", { name: "Database sync details" });

    await sync.click();
    await expect(syncDialog).toBeVisible();
    await startDialogObservation(page);
    await sync.click();
    await page.getByRole("button", { name: "Add agent" }).click();
    await expect(page.getByRole("dialog", { name: "Add agent" })).toBeVisible();
    await page.waitForTimeout(200);

    const observed = await finishDialogObservation(page);
    expect(observed).toEqual({ current: 1, max: 1, overlapping: [] });
  });

  test("requires a document name before configuration deletion", async ({ page }) => {
    await gotoHarness(page);
    await openConfig(page);
    await openConfigSection(page, /^Contexts\b/);
    await page.getByRole("link", { name: /Default context/ }).click();

    await expect(page.getByRole("combobox", { name: "Skills" })).toBeVisible();
    await expect(page.getByRole("textbox", { name: "Tags" })).toBeVisible();
    await page.getByRole("button", { name: "Delete context" }).click();
    const confirm = page.getByRole("textbox", {
      name: "Type Default context to confirm",
    });
    await expect(confirm).toBeFocused();
    await expect(page.getByRole("button", { name: "Delete context" })).toBeDisabled();
    await confirm.fill("Default context");
    await expect(page.getByRole("button", { name: "Delete context" })).toBeEnabled();
    await page.getByRole("button", { name: "Cancel" }).click();
    await expect(confirm).toHaveCount(0);
  });

  test("seeded skills satisfy the canonical editor shape", async ({ page }) => {
    await gotoHarness(page);
    await openConfig(page);
    await openConfigSection(page, /^Skills\b/);
    await page.getByRole("link", { name: /Fleet summary/ }).click();

    await expect(page.getByRole("textbox", { name: "Tool dependencies" })).toHaveValue(
      "mcp-observability.fleet_status",
    );
    await expect(page.getByRole("textbox", { name: "Tags" })).toBeVisible();
    await expect(page.getByTestId("error-banner")).toHaveCount(0);
  });

  test("session context details have an explicit close control", async ({ page }) => {
    await gotoHarness(page);
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /introduction-and-greetings/ })
      .click();
    await page.getByTestId("context-meter").click();
    await expect(page.getByTestId("context-details")).toBeVisible();

    await page.getByRole("button", { name: "Close context details" }).click();
    await expect(page.getByTestId("context-details")).not.toBeVisible();
  });

  test("condensed session behavior details are keyboard accessible", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 800, height: 300 });
    await gotoHarness(page, "coding");
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /introduction-and-greetings/ })
      .click();
    const viewport = page
      .getByTestId("session-screen")
      .locator("[data-slot=scroll-area-viewport]")
      .last();
    await viewport.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
    });

    const trigger = page.getByRole("button", { name: "About Default behavior" });
    await expect(trigger).toBeVisible();
    await trigger.focus();
    await expect(trigger).toBeFocused();
    await expect(page.getByTestId("behaviour-hover-card")).toBeVisible();
  });

  test("context and sync popovers never overlap as dialog portals", async ({
    page,
  }) => {
    await gotoHarness(page);
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /introduction-and-greetings/ })
      .click();
    await page.addStyleTag({
      content:
        '[data-slot="popover-content"] { animation-duration: 600ms !important; }',
    });
    await startDialogObservation(page);

    await page.getByTestId("context-meter").click();
    await expect(page.getByTestId("context-details")).toBeVisible();
    const closingDuration = await page
      .getByTestId("context-details")
      .evaluate((element) => getComputedStyle(element).animationDuration);
    expect(closingDuration).toBe("0.6s");
    await page.getByRole("button", { name: /Sync healthy/ }).click();
    await expect(
      page.getByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();
    await page.waitForTimeout(200);

    const observed = await finishDialogObservation(page);
    expect(observed).toEqual({ current: 1, max: 1, overlapping: [] });
  });

  test("mailbox is reachable from the rail", async ({ page }) => {
    await gotoHarness(page);
    const mailbox = page.getByRole("link", { name: "Mailbox" });
    if (!(await mailbox.first().isVisible())) {
      await page.getByRole("button", { name: "Menu" }).click();
    }
    await page.getByRole("link", { name: "Mailbox" }).last().click();
    await expect(page.getByTestId("mailbox-screen")).toBeVisible();
  });

  test("keeps dialogs inside short windows", async ({ page }) => {
    await page.setViewportSize({ width: 800, height: 300 });
    await gotoHarness(page);
    await page.getByLabel("breadcrumb").getByRole("link", { name: "Agents" }).click();
    await page.getByRole("button", { name: "Add agent" }).click();

    const dialog = page.getByRole("dialog", { name: "Add agent" });
    await expect(dialog).toBeVisible();
    await expect(dialog).toHaveCSS("max-height", "268px");
    await expect(dialog).toHaveCSS("overflow-y", "auto");
    await dialog.evaluate((element) =>
      element.getAnimations().forEach((animation) => animation.finish()),
    );
    const bounds = await dialog.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.y).toBeGreaterThanOrEqual(15);
    expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(285);
  });

  test("contains mailbox text and preserves horizontal code scrolling on phones", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await gotoHarness(page, "mailbox-overflow");
    await page.getByRole("button", { name: "Menu" }).click();
    await page.getByRole("link", { name: "Mailbox" }).click();
    await expect(page.getByTestId("mailbox-screen")).toBeVisible();
    await expectNoPageHorizontalOverflow(page);

    const payload = page.getByTestId("mailbox-screen").locator("pre");
    await expect(payload).toBeVisible();
    const widths = await payload.evaluate((element) => ({
      lineWidth: element.scrollWidth,
      viewportWidth: element.parentElement?.clientWidth ?? 0,
      whiteSpace: getComputedStyle(element).whiteSpace,
    }));
    expect(widths.lineWidth).toBeGreaterThan(widths.viewportWidth);
    expect(widths.whiteSpace).toBe("pre");
  });

  test("empty fleet is the first-run setup", async ({ page }) => {
    await gotoHarness(page, "empty-fleet");
    await expect(page.getByTestId("setup-screen")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Let’s get set up" })).toBeVisible();
  });

  test("bridge failure stays on the startup retry screen", async ({ page }) => {
    await gotoHarness(page, "bridge-unavailable");
    await expect(page.getByTestId("startup-screen")).toBeVisible();
    await expect(page.getByTestId("startup-retry")).toBeVisible();
  });

  test("offline transport keeps the locally cached conversation visible", async ({
    page,
  }) => {
    await gotoHarness(page, "sync-offline");
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /introduction-and-greetings/ })
      .click();

    await expect(page.getByTestId("session-screen")).toBeVisible();
    await expect(
      page.getByText(/seeded turn gives the transcript a stable row/),
    ).toBeVisible();
  });

  test("reopens a conversation after using a different behavior", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await page.getByRole("button", { name: "Behaviour" }).click();
    await page.getByRole("option", { name: /Ops/ }).click();
    await composer(page).fill("inspect the runtime");
    await sendButton(page).click();
    await expect(page.getByText(/received "inspect the runtime"/)).toBeVisible();

    await page
      .getByTestId("session-screen")
      .getByRole("link", { name: "Sessions" })
      .last()
      .click();
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /introduction-and-greetings/ })
      .click();

    await expect(
      page.getByText(/seeded turn gives the transcript a stable row/),
    ).toBeVisible();

    await page
      .getByTestId("session-screen")
      .getByRole("link", { name: "Sessions" })
      .last()
      .click();
    await page
      .getByTestId("sessions-screen")
      .getByRole("link", { name: /inspect the runtime/ })
      .click();
    await expect(page.getByText(/received "inspect the runtime"/)).toBeVisible();
  });
});
