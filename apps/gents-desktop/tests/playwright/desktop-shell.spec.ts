import {
  composer,
  enabledButtonsWithoutAccessibleNames,
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openConfig,
  primarySurfaceCount,
  sendButton,
  test,
} from "./desktopTest";

test.describe("kit shell", () => {
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
    await page.evaluate(() => {
      const counts: number[] = [];
      const record = () =>
        counts.push(document.querySelectorAll('[role="dialog"]').length);
      record();
      const observer = new MutationObserver(record);
      observer.observe(document.documentElement, { childList: true, subtree: true });
      Object.assign(window, {
        __gentsDialogTransition: { counts, observer },
      });
    });

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

    const observed = await page.evaluate(() => {
      const transition = (
        window as typeof window & {
          __gentsDialogTransition: {
            counts: number[];
            observer: MutationObserver;
          };
        }
      ).__gentsDialogTransition;
      transition.observer.disconnect();
      const current = document.querySelectorAll('[role="dialog"]').length;
      return { current, max: Math.max(...transition.counts, current) };
    });
    expect(observed).toEqual({ current: 1, max: 1 });
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
